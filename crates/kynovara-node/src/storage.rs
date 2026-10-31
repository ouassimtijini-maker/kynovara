//! Stockage des blocs sur disque.
//!
//! Les blocs sont ajoutés à la suite dans des fichiers `blk00000.dat`,
//! `blk00001.dat`… (128 Mio maximum chacun). Chaque enregistrement :
//!
//! | magique (4) | longueur (4) | contrôle (8) = SHA-256(données)[..8] | données |
//!
//! Chaque écriture est suivie d'une synchronisation sur disque. Si une
//! écriture échoue (disque plein, erreur matérielle), le fichier est ramené à
//! sa dernière position sûre : le stockage reste utilisable. Après une coupure
//! de courant, un enregistrement incomplet en fin du dernier fichier est
//! détecté et tronqué à l'ouverture. L'index est reconstruit en mémoire au
//! démarrage en relisant les fichiers.

use kynovara_core::block::{Block, HEADER_SIZE, MAX_BLOCK_SIZE};
use kynovara_core::chain::BlockStore;
use kynovara_core::encode::{Decode, Encode};
use kynovara_core::hash::{Hash256, sha256, sha256d};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const RECORD_HEADER: usize = 4 + 4 + 8;
const MAX_FILE_SIZE: u64 = 128 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
struct Location {
    file: u32,
    /// Position du début de l'enregistrement (en-tête compris).
    offset: u64,
    len: u32,
}

/// Panne d'écriture simulée (tests uniquement).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
enum Fault {
    None,
    /// Seuls les `n` premiers octets sont écrits, puis erreur.
    PartialWrite(usize),
    /// Tout est écrit, mais la synchronisation sur disque échoue.
    SyncFailure,
}

/// Stockage de blocs dans des fichiers.
#[derive(Debug)]
pub struct FileBlockStore {
    dir: PathBuf,
    magic: [u8; 4],
    locations: HashMap<Hash256, Location>,
    current_file: u32,
    current_size: u64,
    writer: Option<File>,
    /// Le fichier n'a pas pu être ramené à une position sûre après une
    /// erreur : plus aucune écriture jusqu'au redémarrage (qui répare).
    broken: Option<String>,
    fault: Fault,
}

/// Emplacement d'un bloc sur le disque. Permet de lire un bloc sans tenir le
/// stockage (donc sans tenir le verrou principal du nœud).
#[derive(Clone, Debug)]
pub struct BlockLocation {
    path: PathBuf,
    offset: u64,
    len: u32,
    magic: [u8; 4],
    hash: Hash256,
}

impl BlockLocation {
    /// Lit les octets encodés du bloc, en vérifiant la somme de contrôle et
    /// l'empreinte de l'en-tête.
    pub fn read(&self) -> Result<Vec<u8>, String> {
        let mut file =
            File::open(&self.path).map_err(|e| format!("ouverture de {} impossible : {e}", self.path.display()))?;
        let file_len = file.metadata().map_err(|e| e.to_string())?.len();
        match read_record(&mut file, self.offset, file_len, self.magic).map_err(|e| e.to_string())? {
            Record::Ok { hash, len, data, .. } if hash == self.hash && len == self.len => Ok(data),
            _ => Err(format!("bloc {} corrompu sur le disque ({})", self.hash, self.path.display())),
        }
    }
}

fn file_path(dir: &Path, n: u32) -> PathBuf {
    dir.join(format!("blk{n:05}.dat"))
}

/// Résultat de la lecture d'un enregistrement.
enum Record {
    Ok { hash: Hash256, len: u32, next: u64, data: Vec<u8> },
    End,
    Damaged(String),
}

fn read_record(file: &mut File, offset: u64, file_len: u64, magic: [u8; 4]) -> io::Result<Record> {
    if offset == file_len {
        return Ok(Record::End);
    }
    // Fichier raccourci par un autre programme pendant que le nœud tourne.
    if offset > file_len {
        return Ok(Record::Damaged("position au-delà de la fin du fichier".into()));
    }
    if file_len - offset < RECORD_HEADER as u64 {
        return Ok(Record::Damaged("en-tête d'enregistrement incomplet".into()));
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut header = [0u8; RECORD_HEADER];
    file.read_exact(&mut header)?;
    if header[..4] != magic {
        return Ok(Record::Damaged("octets magiques incorrects".into()));
    }
    let len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    if (len as usize) < HEADER_SIZE || len as usize > MAX_BLOCK_SIZE {
        return Ok(Record::Damaged(format!("longueur invalide ({len})")));
    }
    if file_len - offset - (RECORD_HEADER as u64) < u64::from(len) {
        return Ok(Record::Damaged("données incomplètes".into()));
    }
    let mut data = vec![0u8; len as usize];
    file.read_exact(&mut data)?;
    if sha256(&data)[..8] != header[8..16] {
        return Ok(Record::Damaged("somme de contrôle incorrecte".into()));
    }
    let hash = sha256d(&data[..HEADER_SIZE]);
    Ok(Record::Ok { hash, len, next: offset + RECORD_HEADER as u64 + u64::from(len), data })
}

/// Existe-t-il un enregistrement intact après la position `offset` ? Cela
/// distingue une fin de fichier interrompue par une coupure (rien d'intact
/// après : on peut tronquer sans rien perdre) d'une corruption au milieu du
/// fichier (des blocs intacts suivent : tronquer les détruirait).
fn intact_record_after(file: &mut File, offset: u64, file_len: u64, magic: [u8; 4]) -> io::Result<bool> {
    let mut chunk = vec![0u8; 64 * 1024];
    let mut pos = offset + 1;
    while pos + RECORD_HEADER as u64 <= file_len {
        file.seek(SeekFrom::Start(pos))?;
        let n = file.read(&mut chunk)?;
        if n < magic.len() {
            break;
        }
        for i in 0..=n - magic.len() {
            if chunk[i..i + magic.len()] == magic
                && matches!(read_record(file, pos + i as u64, file_len, magic)?, Record::Ok { .. })
            {
                return Ok(true);
            }
        }
        // Recouvrement : des octets magiques à cheval sur deux morceaux sont vus.
        pos += (n - (magic.len() - 1)) as u64;
    }
    Ok(false)
}

/// Écrit un enregistrement complet puis le synchronise (pannes simulées en test).
fn write_record(writer: &mut File, record: &[u8], fault: Fault) -> io::Result<()> {
    match fault {
        Fault::None => {}
        Fault::PartialWrite(n) => {
            writer.write_all(&record[..n.min(record.len())])?;
            return Err(io::Error::other("écriture partielle (panne simulée)"));
        }
        Fault::SyncFailure => {
            writer.write_all(record)?;
            return Err(io::Error::other("synchronisation impossible (panne simulée)"));
        }
    }
    writer.write_all(record)?;
    writer.sync_data()
}

impl FileBlockStore {
    /// Ouvre (ou crée) le stockage. Renvoie aussi l'empreinte de chaque bloc
    /// stocké, dans l'ordre d'écriture (les parents avant les enfants).
    pub fn open(dir: &Path, magic: [u8; 4]) -> Result<(FileBlockStore, Vec<Hash256>), String> {
        fs::create_dir_all(dir).map_err(|e| format!("création de {} impossible : {e}", dir.display()))?;
        let mut numbers = Vec::new();
        for entry in fs::read_dir(dir).map_err(|e| format!("lecture de {} impossible : {e}", dir.display()))? {
            let name = entry.map_err(|e| e.to_string())?.file_name();
            let name = name.to_string_lossy();
            if let Some(n) = name.strip_prefix("blk").and_then(|r| r.strip_suffix(".dat"))
                && let Ok(n) = n.parse::<u32>()
            {
                numbers.push(n);
            }
        }
        numbers.sort_unstable();
        for (expected, n) in numbers.iter().enumerate() {
            if *n as usize != expected {
                return Err(format!("fichier de blocs manquant : {}", file_path(dir, expected as u32).display()));
            }
        }

        let mut store = FileBlockStore {
            dir: dir.to_path_buf(),
            magic,
            locations: HashMap::new(),
            current_file: 0,
            current_size: 0,
            writer: None,
            broken: None,
            fault: Fault::None,
        };
        let mut order = Vec::new();
        let last = numbers.last().copied();
        for n in numbers {
            let path = file_path(dir, n);
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .map_err(|e| format!("ouverture de {} impossible : {e}", path.display()))?;
            let file_len = file.metadata().map_err(|e| e.to_string())?.len();
            let mut offset = 0u64;
            loop {
                match read_record(&mut file, offset, file_len, magic).map_err(|e| e.to_string())? {
                    Record::End => break,
                    Record::Ok { hash, len, next, .. } => {
                        if let std::collections::hash_map::Entry::Vacant(slot) = store.locations.entry(hash) {
                            slot.insert(Location { file: n, offset, len });
                            order.push(hash);
                        }
                        offset = next;
                    }
                    Record::Damaged(why) => {
                        // Seule une fin de fichier interrompue (coupure pendant une
                        // écriture du dernier fichier) est réparée en tronquant. Une
                        // corruption suivie de blocs intacts n'est jamais tronquée :
                        // ce serait détruire des données encore bonnes.
                        let torn_tail = Some(n) == last
                            && !intact_record_after(&mut file, offset, file_len, magic).map_err(|e| e.to_string())?;
                        if torn_tail {
                            crate::warn!(
                                "stockage",
                                "{} : enregistrement abîmé à la position {offset} ({why}) ; fichier tronqué",
                                path.display()
                            );
                            file.set_len(offset).map_err(|e| e.to_string())?;
                            file.sync_all().map_err(|e| e.to_string())?;
                            break;
                        }
                        return Err(format!(
                            "{} est corrompu à la position {offset} ({why}). Supprime le dossier « blocks » pour resynchroniser.",
                            path.display()
                        ));
                    }
                }
            }
            store.current_file = n;
            store.current_size = offset;
        }
        Ok((store, order))
    }

    pub fn len(&self) -> usize {
        self.locations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.locations.is_empty()
    }

    /// Emplacement d'un bloc stocké.
    pub fn location(&self, hash: &Hash256) -> Option<BlockLocation> {
        let loc = self.locations.get(hash)?;
        Some(BlockLocation {
            path: file_path(&self.dir, loc.file),
            offset: loc.offset,
            len: loc.len,
            magic: self.magic,
            hash: *hash,
        })
    }

    fn writer(&mut self) -> io::Result<&mut File> {
        if self.writer.is_none() {
            let file = OpenOptions::new().create(true).append(true).open(file_path(&self.dir, self.current_file))?;
            self.writer = Some(file);
        }
        self.writer.as_mut().ok_or_else(|| io::Error::other("fichier de blocs indisponible"))
    }

    fn append(&mut self, hash: &Hash256, data: &[u8]) -> io::Result<()> {
        let record_len = (RECORD_HEADER + data.len()) as u64;
        if self.current_size > 0 && self.current_size + record_len > MAX_FILE_SIZE {
            self.writer = None;
            self.current_file += 1;
            self.current_size = 0;
        }
        let offset = self.current_size;
        let file_no = self.current_file;
        let mut record = Vec::with_capacity(RECORD_HEADER + data.len());
        record.extend_from_slice(&self.magic);
        record.extend_from_slice(&(data.len() as u32).to_le_bytes());
        record.extend_from_slice(&sha256(data)[..8]);
        record.extend_from_slice(data);
        let fault = std::mem::replace(&mut self.fault, Fault::None);
        let result = self.writer().and_then(|writer| write_record(writer, &record, fault));
        if let Err(e) = result {
            // Retour à la dernière position sûre : ce qui a pu être écrit est
            // effacé, et la position suivante reste exacte.
            self.writer = None;
            let path = file_path(&self.dir, file_no);
            let repaired = OpenOptions::new().write(true).open(&path).and_then(|f| {
                f.set_len(offset)?;
                f.sync_all()
            });
            if let Err(repair) = repaired {
                self.broken = Some(format!("{} n'a pas pu être réparé après une erreur ({repair})", path.display()));
            }
            return Err(e);
        }
        self.current_size += record_len;
        self.locations.insert(*hash, Location { file: file_no, offset, len: data.len() as u32 });
        Ok(())
    }
}

impl BlockStore for FileBlockStore {
    fn put(&mut self, hash: &Hash256, block: &Block) -> Result<(), String> {
        if self.locations.contains_key(hash) {
            return Ok(());
        }
        if let Some(why) = &self.broken {
            return Err(format!("stockage hors service : {why} ; redémarre le nœud"));
        }
        let data = block.to_bytes();
        self.append(hash, &data).map_err(|e| format!("écriture du bloc {hash} impossible : {e}"))
    }

    fn get(&self, hash: &Hash256) -> Result<Option<Block>, String> {
        let Some(location) = self.location(hash) else {
            return Ok(None);
        };
        let data = location.read()?;
        Block::from_bytes(&data).map(Some).map_err(|e| format!("bloc {hash} illisible : {e}"))
    }

    fn contains(&self, hash: &Hash256) -> bool {
        self.locations.contains_key(hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kynovara_core::genesis::genesis_block;
    use kynovara_core::params::Network;
    use kynovara_core::tx::Transaction;

    fn temp_dir(name: &str) -> PathBuf {
        let mut bytes = [0u8; 8];
        getrandom::getrandom(&mut bytes).unwrap();
        let dir = std::env::temp_dir().join(format!("kynovara-test-{name}-{}", u64::from_le_bytes(bytes)));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn blocks(n: u8) -> Vec<Block> {
        let base = genesis_block(Network::Regtest.params());
        (0..n)
            .map(|i| {
                let mut b = base.clone();
                b.header.nonce = u64::from(i) + 100;
                b.transactions.push(Transaction::coinbase(u64::from(i), vec![i], vec![]));
                b
            })
            .collect()
    }

    const MAGIC: [u8; 4] = *b"KYNR";

    #[test]
    fn write_reopen_read() {
        let dir = temp_dir("rw");
        let bs = blocks(5);
        {
            let (mut store, order) = FileBlockStore::open(&dir, MAGIC).unwrap();
            assert!(order.is_empty());
            for b in &bs {
                store.put(&b.hash(), b).unwrap();
                store.put(&b.hash(), b).unwrap(); // idempotent
            }
            assert_eq!(store.len(), 5);
        }
        let (store, order) = FileBlockStore::open(&dir, MAGIC).unwrap();
        assert_eq!(order, bs.iter().map(Block::hash).collect::<Vec<_>>());
        for b in &bs {
            assert_eq!(store.get(&b.hash()).unwrap().unwrap(), *b);
        }
        assert_eq!(store.get(&Hash256([9; 32])).unwrap(), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn truncated_tail_is_recovered() {
        let dir = temp_dir("trunc");
        let bs = blocks(3);
        {
            let (mut store, _) = FileBlockStore::open(&dir, MAGIC).unwrap();
            for b in &bs {
                store.put(&b.hash(), b).unwrap();
            }
        }
        // Simule une coupure au milieu de l'écriture du dernier bloc.
        let path = file_path(&dir, 0);
        let len = fs::metadata(&path).unwrap().len();
        let f = OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(len - 10).unwrap();
        drop(f);
        let (mut store, order) = FileBlockStore::open(&dir, MAGIC).unwrap();
        assert_eq!(order.len(), 2);
        // On peut réécrire le bloc perdu ensuite.
        store.put(&bs[2].hash(), &bs[2]).unwrap();
        drop(store);
        let (_, order) = FileBlockStore::open(&dir, MAGIC).unwrap();
        assert_eq!(order.len(), 3);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn corruption_is_detected() {
        let dir = temp_dir("corrupt");
        let bs = blocks(2);
        {
            let (mut store, _) = FileBlockStore::open(&dir, MAGIC).unwrap();
            for b in &bs {
                store.put(&b.hash(), b).unwrap();
            }
            // Altère un octet du premier bloc sur le disque.
            let path = file_path(&dir, 0);
            let mut data = fs::read(&path).unwrap();
            data[RECORD_HEADER + 40] ^= 0xff;
            fs::write(&path, data).unwrap();
            assert!(store.get(&bs[0].hash()).is_err());
        }
        // À la réouverture : la corruption est signalée, et le fichier n'est
        // PAS tronqué (le second bloc, intact, serait perdu).
        let path = file_path(&dir, 0);
        let before = fs::metadata(&path).unwrap().len();
        let err = FileBlockStore::open(&dir, MAGIC).unwrap_err();
        assert!(err.contains("corrompu"), "{err}");
        assert_eq!(fs::metadata(&path).unwrap().len(), before);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Non-régression (audit M-2) : une écriture ratée (partielle, ou complète
    /// mais non synchronisée) ne doit pas décaler les positions des blocs
    /// suivants ni empêcher le redémarrage.
    #[test]
    fn failed_write_leaves_store_usable() {
        for fault in [Fault::PartialWrite(40), Fault::SyncFailure] {
            let dir = temp_dir("writefail");
            let bs = blocks(3);
            {
                let (mut store, _) = FileBlockStore::open(&dir, MAGIC).unwrap();
                store.put(&bs[0].hash(), &bs[0]).unwrap();
                store.fault = fault;
                assert!(store.put(&bs[1].hash(), &bs[1]).is_err(), "{fault:?}");
                assert!(!store.contains(&bs[1].hash()));
                // Les écritures suivantes sont lisibles à leur position exacte.
                store.put(&bs[2].hash(), &bs[2]).unwrap();
                assert_eq!(store.get(&bs[2].hash()).unwrap().unwrap(), bs[2]);
                // Le bloc raté peut être réécrit plus tard.
                store.put(&bs[1].hash(), &bs[1]).unwrap();
                assert_eq!(store.get(&bs[1].hash()).unwrap().unwrap(), bs[1]);
            }
            // Le redémarrage relit tout, sans corruption.
            let (store, order) = FileBlockStore::open(&dir, MAGIC).unwrap();
            assert_eq!(order, vec![bs[0].hash(), bs[2].hash(), bs[1].hash()], "{fault:?}");
            for b in &bs {
                assert_eq!(store.get(&b.hash()).unwrap().unwrap(), *b);
            }
            fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn location_reads_without_the_store() {
        let dir = temp_dir("location");
        let bs = blocks(2);
        let (mut store, _) = FileBlockStore::open(&dir, MAGIC).unwrap();
        for b in &bs {
            store.put(&b.hash(), b).unwrap();
        }
        let location = store.location(&bs[1].hash()).unwrap();
        drop(store);
        assert_eq!(location.read().unwrap(), bs[1].to_bytes());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn damaged_last_record_is_truncated() {
        let dir = temp_dir("lastbad");
        let bs = blocks(3);
        {
            let (mut store, _) = FileBlockStore::open(&dir, MAGIC).unwrap();
            for b in &bs {
                store.put(&b.hash(), b).unwrap();
            }
        }
        // Le dernier enregistrement est complet mais abîmé (écriture
        // désordonnée lors d'une coupure) : rien d'intact après, on tronque.
        let path = file_path(&dir, 0);
        let mut data = fs::read(&path).unwrap();
        let n = data.len();
        data[n - 3] ^= 0xff;
        fs::write(&path, data).unwrap();
        let (_, order) = FileBlockStore::open(&dir, MAGIC).unwrap();
        assert_eq!(order, bs[..2].iter().map(Block::hash).collect::<Vec<_>>());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_file_is_an_error() {
        let dir = temp_dir("missing");
        fs::create_dir_all(&dir).unwrap();
        fs::write(file_path(&dir, 1), b"").unwrap();
        assert!(FileBlockStore::open(&dir, MAGIC).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
