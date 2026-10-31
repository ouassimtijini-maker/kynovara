//! Protocole réseau pair-à-pair : messages et découpage en trames.
//!
//! Trame :
//!
//! | champ          | taille | contenu                                  |
//! |----------------|--------|------------------------------------------|
//! | magique        | 4      | identifie le réseau (testnet, regtest…)  |
//! | commande       | 1      | type de message                          |
//! | longueur       | 4      | taille de la charge utile (≤ maximum)    |
//! | contrôle       | 4      | 4 premiers octets de SHA-256d(charge)    |
//! | charge utile   | n      | message encodé                           |

use kynovara_core::block::{Block, MAX_BLOCK_SIZE};
use kynovara_core::chain::MAX_LOCATOR;
use kynovara_core::checkpoint::{Checkpoint, Revocation};
use kynovara_core::encode::{Decode, DecodeError, Encode, Reader, put_len, put_u8, put_u16, put_u32, put_u64};
use kynovara_core::hash::{Hash256, sha256d};
use kynovara_core::tx::Transaction;
use std::fmt;
use std::io::{self, Read};
use std::net::{IpAddr, Ipv6Addr, SocketAddr};

/// Version du protocole.
pub const PROTOCOL_VERSION: u32 = 2;
/// Première version qui comprend les points de contrôle signés (`checkpoint`,
/// `revocation`) : ils ne sont envoyés qu'à ces pairs.
pub const CHECKPOINT_PROTOCOL_VERSION: u32 = 2;
/// Plus ancienne version acceptée.
pub const MIN_PROTOCOL_VERSION: u32 = 1;
/// Taille maximale d'une charge utile.
pub const MAX_PAYLOAD: usize = MAX_BLOCK_SIZE + 4096;
/// Taille de l'en-tête de trame.
pub const FRAME_HEADER: usize = 13;
/// Nombre maximal d'éléments dans un inventaire ou une demande.
pub const MAX_INV: usize = 2_000;
/// Nombre maximal d'adresses par message.
pub const MAX_ADDR: usize = 1_000;
/// Longueur maximale de l'identifiant logiciel.
pub const MAX_USER_AGENT: usize = 64;
/// Nombre de blocs annoncés en réponse à `GetBlocks`.
pub const GETBLOCKS_BATCH: usize = 500;

const CMD_VERSION: u8 = 0;
const CMD_VERACK: u8 = 1;
const CMD_PING: u8 = 2;
const CMD_PONG: u8 = 3;
const CMD_GETBLOCKS: u8 = 4;
const CMD_INV: u8 = 5;
const CMD_GETDATA: u8 = 6;
const CMD_NOTFOUND: u8 = 7;
const CMD_BLOCK: u8 = 8;
const CMD_TX: u8 = 9;
const CMD_GETADDR: u8 = 10;
const CMD_ADDR: u8 = 11;
const CMD_MEMPOOL: u8 = 12;
const CMD_CHECKPOINT: u8 = 13;
const CMD_REVOCATION: u8 = 14;

/// Message de présentation (premier message de chaque connexion).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionMessage {
    pub version: u32,
    /// Bloc de genèse : deux nœuds de réseaux différents se séparent aussitôt.
    pub genesis: Hash256,
    pub best_height: u64,
    /// Nombre aléatoire pour détecter une connexion à soi-même.
    pub nonce: u64,
    /// Port d'écoute (0 si le nœud n'accepte pas de connexions entrantes).
    pub listen_port: u16,
    pub timestamp: u64,
    pub user_agent: String,
}

/// Type d'objet annoncé ou demandé.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InvKind {
    Tx,
    Block,
}

/// Élément d'inventaire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InvItem {
    pub kind: InvKind,
    pub hash: Hash256,
}

/// Message du protocole.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    Version(VersionMessage),
    Verack,
    Ping(u64),
    Pong(u64),
    /// Demande les blocs qui suivent le localisateur.
    GetBlocks {
        locator: Vec<Hash256>,
        stop: Hash256,
    },
    Inv(Vec<InvItem>),
    GetData(Vec<InvItem>),
    NotFound(Vec<InvItem>),
    Block(Box<Block>),
    Tx(Box<Transaction>),
    GetAddr,
    Addr(Vec<SocketAddr>),
    /// Demande l'annonce (`inv`) des transactions en attente du pair. Envoyé
    /// une fois synchronisé : avant, ces transactions ne seraient pas encore
    /// vérifiables.
    Mempool,
    /// Point de contrôle signé (voir `kynovara_core::checkpoint`).
    Checkpoint(Box<Checkpoint>),
    /// Révocation d'une clé de signature par la clé maîtresse.
    Revocation(Box<Revocation>),
}

impl Message {
    /// Nom lisible (journaux).
    pub fn name(&self) -> &'static str {
        match self {
            Message::Version(_) => "version",
            Message::Verack => "verack",
            Message::Ping(_) => "ping",
            Message::Pong(_) => "pong",
            Message::GetBlocks { .. } => "getblocks",
            Message::Inv(_) => "inv",
            Message::GetData(_) => "getdata",
            Message::NotFound(_) => "notfound",
            Message::Block(_) => "block",
            Message::Tx(_) => "tx",
            Message::GetAddr => "getaddr",
            Message::Addr(_) => "addr",
            Message::Mempool => "mempool",
            Message::Checkpoint(_) => "checkpoint",
            Message::Revocation(_) => "revocation",
        }
    }

    fn command(&self) -> u8 {
        match self {
            Message::Version(_) => CMD_VERSION,
            Message::Verack => CMD_VERACK,
            Message::Ping(_) => CMD_PING,
            Message::Pong(_) => CMD_PONG,
            Message::GetBlocks { .. } => CMD_GETBLOCKS,
            Message::Inv(_) => CMD_INV,
            Message::GetData(_) => CMD_GETDATA,
            Message::NotFound(_) => CMD_NOTFOUND,
            Message::Block(_) => CMD_BLOCK,
            Message::Tx(_) => CMD_TX,
            Message::GetAddr => CMD_GETADDR,
            Message::Addr(_) => CMD_ADDR,
            Message::Mempool => CMD_MEMPOOL,
            Message::Checkpoint(_) => CMD_CHECKPOINT,
            Message::Revocation(_) => CMD_REVOCATION,
        }
    }

    fn encode_payload(&self, out: &mut Vec<u8>) {
        match self {
            Message::Version(v) => {
                put_u32(out, v.version);
                out.extend_from_slice(&v.genesis.0);
                put_u64(out, v.best_height);
                put_u64(out, v.nonce);
                put_u16(out, v.listen_port);
                put_u64(out, v.timestamp);
                let ua = v.user_agent.as_bytes();
                let ua = &ua[..ua.len().min(MAX_USER_AGENT)];
                put_len(out, ua.len());
                out.extend_from_slice(ua);
            }
            Message::Verack | Message::GetAddr | Message::Mempool => {}
            Message::Ping(n) | Message::Pong(n) => put_u64(out, *n),
            Message::GetBlocks { locator, stop } => {
                put_len(out, locator.len());
                for h in locator {
                    out.extend_from_slice(&h.0);
                }
                out.extend_from_slice(&stop.0);
            }
            Message::Inv(items) | Message::GetData(items) | Message::NotFound(items) => {
                put_len(out, items.len());
                for item in items {
                    put_u8(
                        out,
                        match item.kind {
                            InvKind::Tx => 1,
                            InvKind::Block => 2,
                        },
                    );
                    out.extend_from_slice(&item.hash.0);
                }
            }
            Message::Block(b) => b.encode_to(out),
            Message::Tx(t) => t.encode_to(out),
            Message::Checkpoint(c) => c.encode_to(out),
            Message::Revocation(r) => r.encode_to(out),
            Message::Addr(addrs) => {
                put_len(out, addrs.len());
                for a in addrs {
                    let ip = match a.ip() {
                        IpAddr::V4(v4) => v4.to_ipv6_mapped(),
                        IpAddr::V6(v6) => v6,
                    };
                    out.extend_from_slice(&ip.octets());
                    put_u16(out, a.port());
                }
            }
        }
    }

    /// Décode une charge utile (qui doit être lue entièrement).
    pub fn decode(command: u8, payload: &[u8]) -> Result<Message, DecodeError> {
        let mut r = Reader::new(payload);
        let msg = match command {
            CMD_VERSION => {
                let version = r.read_u32()?;
                let genesis = Hash256(r.read_array()?);
                let best_height = r.read_u64()?;
                let nonce = r.read_u64()?;
                let listen_port = r.read_u16()?;
                let timestamp = r.read_u64()?;
                let len = r.read_len(MAX_USER_AGENT, 1)?;
                let raw = r.read_bytes(len)?;
                if !raw.iter().all(|c| (0x20..0x7f).contains(c)) {
                    return Err(DecodeError::InvalidValue("identifiant logiciel non imprimable"));
                }
                let user_agent = String::from_utf8_lossy(raw).into_owned();
                Message::Version(VersionMessage {
                    version,
                    genesis,
                    best_height,
                    nonce,
                    listen_port,
                    timestamp,
                    user_agent,
                })
            }
            CMD_VERACK => Message::Verack,
            CMD_PING => Message::Ping(r.read_u64()?),
            CMD_PONG => Message::Pong(r.read_u64()?),
            CMD_GETBLOCKS => {
                let count = r.read_len(MAX_LOCATOR, 32)?;
                let mut locator = Vec::with_capacity(count);
                for _ in 0..count {
                    locator.push(Hash256(r.read_array()?));
                }
                let stop = Hash256(r.read_array()?);
                Message::GetBlocks { locator, stop }
            }
            CMD_INV | CMD_GETDATA | CMD_NOTFOUND => {
                let count = r.read_len(MAX_INV, 33)?;
                let mut items = Vec::with_capacity(count);
                for _ in 0..count {
                    let kind = match r.read_u8()? {
                        1 => InvKind::Tx,
                        2 => InvKind::Block,
                        _ => return Err(DecodeError::InvalidValue("type d'inventaire inconnu")),
                    };
                    items.push(InvItem { kind, hash: Hash256(r.read_array()?) });
                }
                match command {
                    CMD_INV => Message::Inv(items),
                    CMD_GETDATA => Message::GetData(items),
                    _ => Message::NotFound(items),
                }
            }
            CMD_BLOCK => Message::Block(Box::new(Block::decode_from(&mut r)?)),
            CMD_TX => Message::Tx(Box::new(Transaction::decode_from(&mut r)?)),
            CMD_GETADDR => Message::GetAddr,
            CMD_MEMPOOL => Message::Mempool,
            CMD_CHECKPOINT => Message::Checkpoint(Box::new(Checkpoint::decode_from(&mut r)?)),
            CMD_REVOCATION => Message::Revocation(Box::new(Revocation::decode_from(&mut r)?)),
            CMD_ADDR => {
                let count = r.read_len(MAX_ADDR, 18)?;
                let mut addrs = Vec::with_capacity(count);
                for _ in 0..count {
                    let ip = Ipv6Addr::from(r.read_array::<16>()?);
                    let port = r.read_u16()?;
                    let ip = match ip.to_ipv4_mapped() {
                        Some(v4) => IpAddr::V4(v4),
                        None => IpAddr::V6(ip),
                    };
                    addrs.push(SocketAddr::new(ip, port));
                }
                Message::Addr(addrs)
            }
            _ => return Err(DecodeError::InvalidValue("commande inconnue")),
        };
        r.finish()?;
        Ok(msg)
    }

    /// Trame complète, prête à envoyer.
    pub fn to_frame(&self, magic: [u8; 4]) -> Vec<u8> {
        let mut payload = Vec::new();
        self.encode_payload(&mut payload);
        raw_frame(magic, self.command(), &payload)
    }
}

/// Trame à partir d'une commande et d'une charge utile déjà encodée.
fn raw_frame(magic: [u8; 4], command: u8, payload: &[u8]) -> Vec<u8> {
    let checksum = sha256d(payload);
    let mut frame = Vec::with_capacity(FRAME_HEADER + payload.len());
    frame.extend_from_slice(&magic);
    frame.push(command);
    frame.extend_from_slice(&u32::try_from(payload.len()).unwrap_or(u32::MAX).to_le_bytes());
    frame.extend_from_slice(&checksum.0[..4]);
    frame.extend_from_slice(payload);
    frame
}

/// Trame `block` à partir des octets d'un bloc déjà encodé (lus tels quels
/// sur le disque : ni décodage ni réencodage).
pub fn block_frame(magic: [u8; 4], block_bytes: &[u8]) -> Vec<u8> {
    raw_frame(magic, CMD_BLOCK, block_bytes)
}

/// Erreur de trame : le pair ne respecte pas le protocole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    BadMagic,
    TooLarge(usize),
    BadChecksum,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::BadMagic => write!(f, "octets magiques incorrects (autre réseau ?)"),
            FrameError::TooLarge(n) => write!(f, "message trop grand ({n} octets)"),
            FrameError::BadChecksum => write!(f, "somme de contrôle incorrecte"),
        }
    }
}

/// Accumule les octets reçus et en extrait les trames complètes. Supporte
/// les lectures partielles (une trame peut arriver en plusieurs morceaux).
#[derive(Debug)]
pub struct FrameReader {
    magic: [u8; 4],
    buf: Vec<u8>,
}

impl FrameReader {
    pub fn new(magic: [u8; 4]) -> Self {
        FrameReader { magic, buf: Vec::new() }
    }

    /// Lit ce qui est disponible sur le flux. Renvoie le nombre d'octets lus
    /// (0 = connexion fermée).
    pub fn read_from<R: Read>(&mut self, stream: &mut R) -> io::Result<usize> {
        let mut chunk = [0u8; 64 * 1024];
        let n = stream.read(&mut chunk)?;
        self.buf.extend_from_slice(&chunk[..n]);
        Ok(n)
    }

    /// Ajoute des octets (tests).
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Extrait la prochaine trame complète, s'il y en a une.
    pub fn next_frame(&mut self) -> Result<Option<(u8, Vec<u8>)>, FrameError> {
        if self.buf.len() < FRAME_HEADER {
            // Vérifie la magie dès que possible pour couper court.
            if self.buf.len() >= 4 && self.buf[..4] != self.magic {
                return Err(FrameError::BadMagic);
            }
            return Ok(None);
        }
        if self.buf[..4] != self.magic {
            return Err(FrameError::BadMagic);
        }
        let command = self.buf[4];
        let mut len_bytes = [0u8; 4];
        len_bytes.copy_from_slice(&self.buf[5..9]);
        let len = u32::from_le_bytes(len_bytes) as usize;
        if len > MAX_PAYLOAD {
            return Err(FrameError::TooLarge(len));
        }
        if self.buf.len() < FRAME_HEADER + len {
            return Ok(None);
        }
        let payload = self.buf[FRAME_HEADER..FRAME_HEADER + len].to_vec();
        if sha256d(&payload).0[..4] != self.buf[9..13] {
            return Err(FrameError::BadChecksum);
        }
        self.buf.drain(..FRAME_HEADER + len);
        Ok(Some((command, payload)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kynovara_core::genesis::genesis_block;
    use kynovara_core::params::Network;
    use std::net::Ipv4Addr;

    const MAGIC: [u8; 4] = *b"TEST";

    fn samples() -> Vec<Message> {
        let block = genesis_block(Network::Regtest.params());
        vec![
            Message::Version(VersionMessage {
                version: 1,
                genesis: Hash256([1; 32]),
                best_height: 42,
                nonce: 7,
                listen_port: 29733,
                timestamp: 1_800_000_000,
                user_agent: "/kynovara:0.1.0/".into(),
            }),
            Message::Verack,
            Message::Ping(5),
            Message::Pong(6),
            Message::GetBlocks { locator: vec![Hash256([2; 32]), Hash256([3; 32])], stop: Hash256::ZERO },
            Message::Inv(vec![
                InvItem { kind: InvKind::Block, hash: Hash256([4; 32]) },
                InvItem { kind: InvKind::Tx, hash: Hash256([5; 32]) },
            ]),
            Message::GetData(vec![InvItem { kind: InvKind::Tx, hash: Hash256([6; 32]) }]),
            Message::NotFound(vec![]),
            Message::Tx(Box::new(block.transactions[0].clone())),
            Message::Block(Box::new(block)),
            Message::GetAddr,
            Message::Addr(vec![
                SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10)), 19733),
                "[2001:db8::1]:9733".parse().unwrap(),
            ]),
            Message::Mempool,
            {
                use kynovara_core::checkpoint::Delegation;
                use kynovara_core::crypto::KeyPair;
                let master = KeyPair::from_secret_bytes(&kynovara_core::params::REGTEST_CHECKPOINT_SECRET);
                let hot = KeyPair::from_secret_bytes(&[3; 32]);
                let d = Delegation::new(&master, &Hash256([1; 32]), hot.public_key(), 9);
                Message::Checkpoint(Box::new(Checkpoint::sign_delegate(
                    &hot,
                    d,
                    &Hash256([1; 32]),
                    8,
                    Hash256([2; 32]),
                )))
            },
            Message::Revocation(Box::new(Revocation {
                signer: kynovara_core::crypto::PublicKey([7; 32]),
                signature: kynovara_core::crypto::Signature([8; 64]),
            })),
        ]
    }

    #[test]
    fn roundtrip_all_messages() {
        for msg in samples() {
            let frame = msg.to_frame(MAGIC);
            let mut reader = FrameReader::new(MAGIC);
            reader.push(&frame);
            let (cmd, payload) = reader.next_frame().unwrap().unwrap();
            assert_eq!(Message::decode(cmd, &payload).unwrap(), msg, "{}", msg.name());
            assert_eq!(reader.next_frame().unwrap(), None);
        }
    }

    #[test]
    fn raw_block_frame_matches_encoded_message() {
        let block = genesis_block(Network::Regtest.params());
        let frame = block_frame(MAGIC, &block.to_bytes());
        assert_eq!(frame, Message::Block(Box::new(block)).to_frame(MAGIC));
    }

    #[test]
    fn frames_arrive_in_pieces() {
        let mut stream = Vec::new();
        for msg in samples() {
            stream.extend(msg.to_frame(MAGIC));
        }
        let mut reader = FrameReader::new(MAGIC);
        let mut decoded = Vec::new();
        for byte in stream {
            reader.push(&[byte]);
            while let Some((cmd, payload)) = reader.next_frame().unwrap() {
                decoded.push(Message::decode(cmd, &payload).unwrap());
            }
        }
        assert_eq!(decoded, samples());
    }

    #[test]
    fn protocol_violations() {
        let frame = Message::Ping(1).to_frame(MAGIC);
        // Mauvais réseau.
        let mut reader = FrameReader::new(*b"AUTR");
        reader.push(&frame);
        assert_eq!(reader.next_frame(), Err(FrameError::BadMagic));
        // Somme de contrôle altérée.
        let mut bad = frame.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        let mut reader = FrameReader::new(MAGIC);
        reader.push(&bad);
        assert_eq!(reader.next_frame(), Err(FrameError::BadChecksum));
        // Taille annoncée excessive : refus avant même d'attendre les données.
        let mut huge = frame;
        huge[5..9].copy_from_slice(&(MAX_PAYLOAD as u32 + 1).to_le_bytes());
        let mut reader = FrameReader::new(MAGIC);
        reader.push(&huge[..FRAME_HEADER]);
        assert!(matches!(reader.next_frame(), Err(FrameError::TooLarge(_))));
        // Commande inconnue, octets en trop, agent non imprimable.
        assert!(Message::decode(99, &[]).is_err());
        assert!(Message::decode(CMD_VERACK, &[0]).is_err());
        let mut v = Vec::new();
        Message::Version(VersionMessage {
            version: 1,
            genesis: Hash256::ZERO,
            best_height: 0,
            nonce: 0,
            listen_port: 0,
            timestamp: 0,
            user_agent: "ok".into(),
        })
        .encode_payload(&mut v);
        let n = v.len();
        v[n - 1] = 0x07;
        assert!(Message::decode(CMD_VERSION, &v).is_err());
    }

    #[test]
    fn random_payloads_never_panic() {
        let mut state = 0x1234_5678_u64;
        for _ in 0..20_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let len = (state % 300) as usize;
            let bytes: Vec<u8> = (0..len).map(|i| (state.rotate_left(i as u32 % 64) as u8) ^ i as u8).collect();
            for cmd in 0..14u8 {
                let _ = Message::decode(cmd, &bytes);
            }
        }
    }
}
