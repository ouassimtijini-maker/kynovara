//! Le nœud : relie la chaîne, la mempool, le réseau, le mineur et le RPC.
//!
//! Ordre des verrous (pour éviter tout interblocage) : `state`, puis `peers`,
//! puis l'état d'un pair, puis `addrbook`/`bans`. On ne prend jamais `state`
//! en tenant un verrou plus bas dans cet ordre.

use crate::config::NodeConfig;
use crate::mining::{self, MinerShared};
use crate::p2p::addrbook::{AddrBook, ip_key, is_usable};
use crate::p2p::message::{
    GETBLOCKS_BATCH, InvItem, InvKind, MAX_ADDR, MAX_INV, MIN_PROTOCOL_VERSION, Message, PROTOCOL_VERSION,
    VersionMessage, block_frame,
};
use crate::p2p::peer::{
    BAN_THRESHOLD, BLOCK_DOWNLOAD_TIMEOUT, BoundedSet, HANDSHAKE_TIMEOUT, INACTIVITY_TIMEOUT, MAX_QUEUED_BYTES,
    PING_INTERVAL, PING_TIMEOUT, Peer, run_peer,
};
use crate::storage::FileBlockStore;
use crate::{debug, info, logger, rpc, warn};
use kynovara_core::block::Block;
use kynovara_core::chain::{BlockStore, Chain, ProcessOutcome, TipChange};
use kynovara_core::hash::Hash256;
use kynovara_core::mempool::{Mempool, MempoolError, MempoolPolicy};
use kynovara_core::params::ConsensusParams;
use kynovara_core::time::now_secs;
use kynovara_core::tx::Transaction;
use kynovara_core::validation::{BlockError, TxError, verify_input_signatures};
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Identifiant logiciel annoncé aux pairs.
pub const USER_AGENT: &str = concat!("/kynovara:", env!("CARGO_PKG_VERSION"), "/");
/// Nombre maximal de blocs demandés et non reçus par pair.
const MAX_INFLIGHT: usize = 512;
/// Durée d'un bannissement.
const BAN_DURATION: Duration = Duration::from_secs(24 * 3600);
/// Connexions entrantes maximales depuis une même adresse IP (ou un même
/// préfixe IPv6 /64).
const MAX_INBOUND_PER_IP: usize = 4;
/// Adresses acceptées au plus sur une même connexion (le carnet limite en plus
/// chaque source, toutes connexions confondues).
const MAX_ADDR_PER_PEER: usize = 1_000;
/// Transactions de la mempool annoncées à un nouveau pair (les mieux payées).
const MEMPOOL_ANNOUNCE: usize = 5_000;
/// Rediffusion des transactions locales tant qu'elles ne sont pas confirmées.
const REBROADCAST_INTERVAL: Duration = Duration::from_secs(300);

/// État protégé par le verrou principal.
pub(crate) struct State {
    pub chain: Chain<FileBlockStore>,
    pub mempool: Mempool,
    /// Transactions récemment refusées, par **wtxid** (signatures comprises :
    /// une copie altérée par un tiers ne fait pas refuser la vraie). Vidé à
    /// chaque nouveau sommet, car un refus de politique (frais, place) peut
    /// ne plus tenir.
    pub rejected: BoundedSet<Hash256>,
}

pub(crate) struct Inner {
    pub params: &'static ConsensusParams,
    pub config: NodeConfig,
    pub state: Mutex<State>,
    pub peers: Mutex<HashMap<u64, Arc<Peer>>>,
    pub addrbook: Mutex<AddrBook>,
    pub bans: Mutex<HashMap<IpAddr, Instant>>,
    /// Pairs ajoutés via `--addnode`, les nœuds d'amorçage ou le RPC.
    pub extra_peers: Mutex<Vec<SocketAddr>>,
    /// Pairs configurés qui se sont révélés être ce nœud lui-même (par exemple
    /// le nœud d'amorçage lancé sur sa propre machine) : plus jamais rejoints.
    self_targets: Mutex<HashSet<SocketAddr>>,
    pub connecting: Mutex<HashSet<SocketAddr>>,
    /// Prochaine tentative autorisée et nombre d'échecs, par pair configuré.
    pub retry: Mutex<HashMap<SocketAddr, (Instant, u32)>>,
    pub miner: MinerShared,
    pub shutdown: AtomicBool,
    /// Nombres envoyés dans nos messages `version` (un par connexion) : en
    /// recevoir un, c'est être connecté à soi-même.
    sent_nonces: Mutex<HashSet<u64>>,
    /// Transactions soumises localement (portefeuille, RPC), rediffusées
    /// jusqu'à leur confirmation.
    local_txs: Mutex<HashSet<Hash256>>,
    /// Points de contrôle signés (voir `checkpoints.rs`).
    pub(crate) checkpoints: Mutex<crate::checkpoints::CheckpointState>,
    next_peer_id: AtomicU64,
    pub p2p_addr: Option<SocketAddr>,
    pub rpc_addr: SocketAddr,
    pub cookie: String,
    pub dir: PathBuf,
    pub started: Instant,
    threads: Mutex<Vec<JoinHandle<()>>>,
    lock_file: Mutex<Option<File>>,
}

/// Poignée vers un nœud en fonctionnement (copiable).
#[derive(Clone)]
pub struct Node {
    pub(crate) inner: Arc<Inner>,
}

impl std::fmt::Debug for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Node")
            .field("network", &self.inner.params.network)
            .field("p2p", &self.inner.p2p_addr)
            .field("rpc", &self.inner.rpc_addr)
            .finish()
    }
}

/// Résultat du traitement d'un bloc par le nœud.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockResult {
    /// Le bloc est devenu (ou fait partie de) la chaîne active.
    NewTip,
    /// Stocké sur une branche secondaire.
    SideChain,
    /// Parent inconnu, mis de côté.
    Orphan,
    AlreadyKnown,
}

pub(crate) fn random_u64() -> u64 {
    let mut b = [0u8; 8];
    // En cas d'échec (extrêmement improbable), on se rabat sur l'horloge.
    if getrandom::getrandom(&mut b).is_err() {
        return now_secs() ^ u64::from(std::process::id()).rotate_left(32);
    }
    u64::from_le_bytes(b)
}

fn spawn_named<F: FnOnce() + Send + 'static>(name: &str, f: F) -> Result<JoinHandle<()>, String> {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(f)
        .map_err(|e| format!("impossible de démarrer le fil « {name} » : {e}"))
}

/// Écrit le fichier d'authentification RPC (lisible par l'utilisateur seul).
fn write_cookie(path: &Path, token: &str) -> Result<(), String> {
    let _ = std::fs::remove_file(path);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|e| format!("écriture de {} impossible : {e}", path.display()))?;
    std::io::Write::write_all(&mut file, token.as_bytes()).map_err(|e| e.to_string())
}

impl Node {
    /// Démarre un nœud : charge la chaîne depuis le disque, ouvre les ports et
    /// lance les fils (réseau, mineur, RPC).
    pub fn start(config: NodeConfig) -> Result<Node, String> {
        logger::set_level(config.log_level);
        let params = config.network.params();
        let dir = config.network_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("création de {} impossible : {e}", dir.display()))?;

        // Un seul nœud par dossier de données.
        let lock_path = dir.join("node.lock");
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|e| format!("ouverture de {} impossible : {e}", lock_path.display()))?;
        if lock_file.try_lock().is_err() {
            return Err(format!(
                "un autre nœud Kynovara utilise déjà le dossier {} (arrête-le ou choisis un autre --datadir)",
                dir.display()
            ));
        }

        let (store, order) = FileBlockStore::open(&dir.join("blocks"), params.magic)?;
        let mut chain = Chain::new(params, store).map_err(|e| e.to_string())?;
        chain.set_side_branch_margin(config.side_branch_margin);
        let genesis = chain.genesis_hash();
        let total = order.len();
        if total > 1 {
            info!("chaîne", "chargement de {total} blocs depuis le disque…");
        }
        let now = now_secs();
        for (i, hash) in order.iter().enumerate() {
            if *hash == genesis {
                continue;
            }
            let block = chain.store().get(hash)?.ok_or_else(|| format!("bloc {hash} introuvable dans le stockage"))?;
            if let Err(e) = chain.process_block(block, now) {
                warn!("chaîne", "bloc stocké {hash} ignoré : {e}");
            }
            if (i + 1) % 5_000 == 0 {
                info!("chaîne", "… {} / {total} blocs vérifiés", i + 1);
            }
        }
        info!("chaîne", "réseau {} : hauteur {}, sommet {}", params.network, chain.height(), chain.tip_hash());

        let rpc_listener = TcpListener::bind(config.rpc)
            .map_err(|e| format!("impossible d'écouter le RPC sur {} : {e}", config.rpc))?;
        let rpc_addr = rpc_listener.local_addr().map_err(|e| e.to_string())?;
        let p2p_listener = match config.listen {
            Some(addr) => {
                Some(TcpListener::bind(addr).map_err(|e| format!("impossible d'écouter les pairs sur {addr} : {e}"))?)
            }
            None => None,
        };
        let p2p_addr = match &p2p_listener {
            Some(l) => Some(l.local_addr().map_err(|e| e.to_string())?),
            None => None,
        };

        let mut token = [0u8; 32];
        getrandom::getrandom(&mut token).map_err(|e| format!("générateur aléatoire indisponible : {e}"))?;
        let cookie = kynovara_core::hex::encode(&token);
        write_cookie(&dir.join(".cookie"), &cookie)?;

        let mut addrbook = AddrBook::new(random_u64());
        addrbook.load(&dir.join("peers.txt"));

        let miner = MinerShared::new(config.mining_threads);
        if let Some(addr) = &config.mining_address {
            miner.configure(*addr, config.mining_threads);
        }
        let start_mining = config.mine;
        let mut extra = config.addnode.clone();
        // Nœuds d'amorçage : seulement si aucun pair n'est imposé (--connect).
        let seeds: Vec<SocketAddr> = if config.connect.is_empty() { config.seeds.clone() } else { Vec::new() };
        for seed in &seeds {
            if !extra.contains(seed) {
                extra.push(*seed);
            }
        }

        let node = Node {
            inner: Arc::new(Inner {
                params,
                config,
                state: Mutex::new(State {
                    chain,
                    mempool: Mempool::new(MempoolPolicy::default()),
                    rejected: BoundedSet::new(50_000),
                }),
                peers: Mutex::new(HashMap::new()),
                addrbook: Mutex::new(addrbook),
                bans: Mutex::new(HashMap::new()),
                extra_peers: Mutex::new(extra),
                self_targets: Mutex::new(HashSet::new()),
                connecting: Mutex::new(HashSet::new()),
                retry: Mutex::new(HashMap::new()),
                miner,
                shutdown: AtomicBool::new(false),
                sent_nonces: Mutex::new(HashSet::new()),
                local_txs: Mutex::new(HashSet::new()),
                checkpoints: Mutex::new(crate::checkpoints::CheckpointState::default()),
                next_peer_id: AtomicU64::new(1),
                p2p_addr,
                rpc_addr,
                cookie,
                dir,
                started: Instant::now(),
                threads: Mutex::new(Vec::new()),
                lock_file: Mutex::new(Some(lock_file)),
            }),
        };

        node.restore_checkpoints();

        let mut threads = Vec::new();
        if let Some(listener) = p2p_listener {
            let n = node.clone();
            threads.push(spawn_named("p2p-accueil", move || accept_loop(&n, &listener))?);
            info!("p2p", "écoute des pairs sur {}", p2p_addr.map_or_else(String::new, |a| a.to_string()));
        }
        if !seeds.is_empty() {
            let list: Vec<String> = seeds.iter().map(ToString::to_string).collect();
            info!("p2p", "nœud(s) d'amorçage : {}", list.join(", "));
        }
        let n = node.clone();
        threads.push(spawn_named("connexions", move || connection_manager(&n))?);
        let n = node.clone();
        threads.push(spawn_named("mineur", move || mining::miner_loop(&n))?);
        let n = node.clone();
        threads.push(spawn_named("rpc", move || rpc::serve(&n, &rpc_listener))?);
        info!("rpc", "interface RPC sur http://{rpc_addr} (jeton : {})", node.inner.dir.join(".cookie").display());
        *node.inner.threads.lock().unwrap_or_else(|e| e.into_inner()) = threads;

        if start_mining {
            node.inner.miner.running.store(true, Ordering::SeqCst);
            info!("mineur", "minage démarré ({} fil(s))", node.inner.miner.threads());
        }
        Ok(node)
    }

    /// Demande l'arrêt du nœud (retour immédiat).
    pub fn shutdown(&self) {
        if self.inner.shutdown.swap(true, Ordering::SeqCst) {
            return;
        }
        info!("nœud", "arrêt en cours…");
        self.inner.miner.running.store(false, Ordering::SeqCst);
        for peer in self.peer_list() {
            peer.disconnect();
        }
    }

    /// Attend la fin de tous les fils principaux (après `shutdown`).
    pub fn wait(&self) {
        while !self.is_shutting_down() {
            std::thread::sleep(Duration::from_millis(200));
        }
        let threads = std::mem::take(&mut *self.inner.threads.lock().unwrap_or_else(|e| e.into_inner()));
        for t in threads {
            let _ = t.join();
        }
        self.addrbook().save(&self.inner.dir.join("peers.txt"));
        let _ = std::fs::remove_file(self.inner.dir.join(".cookie"));
        // Libère le verrou du dossier de données.
        self.inner.lock_file.lock().unwrap_or_else(|e| e.into_inner()).take();
        info!("nœud", "arrêté");
    }

    pub fn is_shutting_down(&self) -> bool {
        self.inner.shutdown.load(Ordering::SeqCst)
    }

    pub fn params(&self) -> &'static ConsensusParams {
        self.inner.params
    }

    /// Adresse d'écoute P2P effective.
    pub fn p2p_addr(&self) -> Option<SocketAddr> {
        self.inner.p2p_addr
    }

    /// Adresse RPC effective.
    pub fn rpc_addr(&self) -> SocketAddr {
        self.inner.rpc_addr
    }

    /// Jeton d'authentification RPC.
    pub fn cookie(&self) -> &str {
        &self.inner.cookie
    }

    /// Dossier de données du réseau.
    pub fn data_dir(&self) -> &Path {
        &self.inner.dir
    }

    /// Verrou principal. Un verrou empoisonné signifie qu'un fil a paniqué au
    /// milieu d'une modification de l'état de consensus : on arrête le nœud
    /// plutôt que de continuer avec un état douteux.
    pub(crate) fn state(&self) -> MutexGuard<'_, State> {
        match self.inner.state.lock() {
            Ok(guard) => guard,
            Err(_) => {
                crate::error!("nœud", "état interne corrompu (panique pendant une mise à jour) : arrêt immédiat");
                std::process::exit(70);
            }
        }
    }

    fn addrbook(&self) -> MutexGuard<'_, AddrBook> {
        self.inner.addrbook.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn next_peer_id(&self) -> u64 {
        self.inner.next_peer_id.fetch_add(1, Ordering::SeqCst)
    }

    pub(crate) fn peer_list(&self) -> Vec<Arc<Peer>> {
        self.inner.peers.lock().unwrap_or_else(|e| e.into_inner()).values().cloned().collect()
    }

    /// Hauteur et sommet de la chaîne active.
    pub fn tip(&self) -> (u64, Hash256) {
        let st = self.state();
        (st.chain.height(), st.chain.tip_hash())
    }

    /// Nombre de pairs ayant terminé la présentation.
    pub fn connected_peers(&self) -> usize {
        self.peer_list().iter().filter(|p| p.info().handshake_done()).count()
    }

    /// Nombre de transactions en mempool.
    pub fn mempool_len(&self) -> usize {
        self.state().mempool.len()
    }

    /// La transaction est-elle en mempool ?
    pub fn mempool_contains(&self, txid: &Hash256) -> bool {
        self.state().mempool.contains(txid)
    }

    /// Hauteur du bloc actif contenant la transaction.
    pub fn tx_height(&self, txid: &Hash256) -> Option<u64> {
        self.state().chain.tx_height(txid)
    }

    /// Vérifie la cohérence complète de l'état (diagnostic, tests).
    pub fn check_consistency(&self) -> Result<(), String> {
        self.state().chain.check_consistency()
    }

    pub(crate) fn register_peer(&self, peer: &Arc<Peer>) -> bool {
        if self.is_shutting_down() {
            return false;
        }
        let mut peers = self.inner.peers.lock().unwrap_or_else(|e| e.into_inner());
        if peer.inbound {
            let inbound = peers.values().filter(|p| p.inbound).count();
            if inbound >= self.inner.config.max_inbound {
                return false;
            }
            // Une même adresse IP (ou un même /64 IPv6), hors machine locale, ne
            // peut pas occuper toutes les places entrantes.
            let ip = peer.addr.ip();
            let key = ip_key(&ip);
            let same_ip = peers.values().filter(|p| p.inbound && ip_key(&p.addr.ip()) == key).count();
            if !ip.is_loopback() && same_ip >= MAX_INBOUND_PER_IP {
                return false;
            }
        }
        peers.insert(peer.id, Arc::clone(peer));
        true
    }

    pub(crate) fn unregister_peer(&self, id: u64) {
        let removed = self.inner.peers.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        if let Some(peer) = &removed {
            self.inner.sent_nonces.lock().unwrap_or_else(|e| e.into_inner()).remove(&peer.nonce);
        }
        if let Some(target) = removed.and_then(|peer| peer.target) {
            // Reconnexion d'un pair configuré après un court délai.
            self.inner
                .retry
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(target, (Instant::now() + Duration::from_secs(5), 0));
        }
    }

    /// Pairs configurés qui ne sont plus rejoints parce que ce sont nous-mêmes.
    pub fn ignored_peers(&self) -> Vec<SocketAddr> {
        let mut v: Vec<SocketAddr> =
            self.inner.self_targets.lock().unwrap_or_else(|e| e.into_inner()).iter().copied().collect();
        v.sort();
        v
    }

    /// Ajoute un pair à joindre (équivalent de `--addnode`).
    pub fn add_peer(&self, addr: SocketAddr) {
        let mut extra = self.inner.extra_peers.lock().unwrap_or_else(|e| e.into_inner());
        if !extra.contains(&addr) {
            extra.push(addr);
        }
        self.inner.retry.lock().unwrap_or_else(|e| e.into_inner()).remove(&addr);
    }

    /// L'adresse (ou son /64 IPv6) est-elle bannie ?
    pub(crate) fn is_banned(&self, ip: &IpAddr) -> bool {
        let bans = self.inner.bans.lock().unwrap_or_else(|e| e.into_inner());
        bans.get(&ip_key(ip)).is_some_and(|until| Instant::now() < *until)
    }

    /// Pénalise un pair ; au-delà du seuil, il est déconnecté et banni pour
    /// 24 h (sauf en boucle locale, où l'on se contente de déconnecter). En
    /// IPv6, c'est le préfixe /64 entier qui est banni : un abonné en dispose
    /// d'au moins un et pourrait sinon changer d'adresse à volonté.
    pub(crate) fn punish(&self, peer: &Peer, score: u32, reason: &str) {
        let total = {
            let mut info = peer.info();
            info.misbehavior = info.misbehavior.saturating_add(score);
            info.misbehavior
        };
        warn!("p2p", "pair {} : {reason} (score {total})", peer.addr);
        if total >= BAN_THRESHOLD {
            let ip = peer.addr.ip();
            if !ip.is_loopback() {
                let key = ip_key(&ip);
                self.inner.bans.lock().unwrap_or_else(|e| e.into_inner()).insert(key, Instant::now() + BAN_DURATION);
                warn!("p2p", "{key} banni pour 24 h");
            }
            peer.disconnect();
        }
    }

    /// Notre message `version` pour la connexion `peer` (avec le nombre
    /// propre à cette connexion, mémorisé pour détecter les connexions à
    /// soi-même).
    pub(crate) fn version_message(&self, peer: &Peer) -> VersionMessage {
        let (height, _) = self.tip();
        self.inner.sent_nonces.lock().unwrap_or_else(|e| e.into_inner()).insert(peer.nonce);
        VersionMessage {
            version: PROTOCOL_VERSION,
            genesis: self.params().genesis.hash.unwrap_or(Hash256::ZERO),
            best_height: height,
            nonce: peer.nonce,
            listen_port: self.inner.p2p_addr.map_or(0, |a| a.port()),
            timestamp: now_secs(),
            user_agent: USER_AGENT.to_string(),
        }
    }

    /// Minuteries d'un pair. Renvoie une raison de déconnexion éventuelle.
    pub(crate) fn check_peer_timers(&self, peer: &Peer) -> Option<String> {
        let now = Instant::now();
        let ping = {
            let mut info = peer.info();
            if !info.handshake_done() && now.duration_since(peer.connected_at) > HANDSHAKE_TIMEOUT {
                return Some("délai de présentation dépassé".into());
            }
            if now.duration_since(info.last_recv) > INACTIVITY_TIMEOUT {
                return Some("pair inactif".into());
            }
            if info.ping.is_some_and(|(_, sent)| now.duration_since(sent) > PING_TIMEOUT) {
                return Some("pas de réponse au ping".into());
            }
            if info.inflight.values().any(|t| now.duration_since(*t) > BLOCK_DOWNLOAD_TIMEOUT) {
                return Some("téléchargement de blocs bloqué".into());
            }
            if info.handshake_done() && info.ping.is_none() && now.duration_since(info.last_ping) > PING_INTERVAL {
                let nonce = random_u64();
                info.ping = Some((nonce, now));
                info.last_ping = now;
                Some(nonce)
            } else {
                None
            }
        };
        if let Some(nonce) = ping {
            peer.send(&Message::Ping(nonce));
        }
        None
    }

    /// Traite un message d'un pair. `Err` = déconnecter (avec la raison).
    pub(crate) fn handle_message(&self, peer: &Arc<Peer>, msg: Message) -> Result<(), String> {
        let handshake_done = peer.info().handshake_done();
        match msg {
            Message::Version(v) => self.on_version(peer, v),
            Message::Verack => self.on_verack(peer),
            _ if !handshake_done => Err("message reçu avant la fin de la présentation".into()),
            Message::Ping(n) => {
                peer.send(&Message::Pong(n));
                Ok(())
            }
            Message::Pong(n) => {
                let mut info = peer.info();
                if info.ping.is_some_and(|(expected, _)| expected == n) {
                    info.ping = None;
                }
                Ok(())
            }
            Message::GetAddr => {
                self.on_getaddr(peer);
                Ok(())
            }
            Message::Mempool => {
                // Servie une fois par connexion (annonce des mieux payées).
                let first = !std::mem::replace(&mut peer.info().answered_mempool, true);
                if first {
                    let txids = self.state().mempool.best_txids(MEMPOOL_ANNOUNCE);
                    self.announce_txs(peer, &txids, false);
                }
                Ok(())
            }
            Message::Addr(addrs) => {
                let allowed = {
                    let mut info = peer.info();
                    let room = MAX_ADDR_PER_PEER.saturating_sub(info.addr_received);
                    let n = addrs.len().min(room).min(MAX_ADDR);
                    info.addr_received += addrs.len();
                    n
                };
                // Le carnet filtre les adresses non routables et limite chaque
                // source, même si elle se reconnecte.
                let source = peer.addr.ip();
                let mut book = self.addrbook();
                for a in addrs.into_iter().take(allowed) {
                    book.add_from(a, source);
                }
                Ok(())
            }
            Message::GetBlocks { locator, stop } => {
                let hashes = self.state().chain.hashes_after_locator(&locator, &stop, GETBLOCKS_BATCH);
                if !hashes.is_empty() {
                    let items = hashes.iter().map(|h| InvItem { kind: InvKind::Block, hash: *h }).collect();
                    {
                        let mut info = peer.info();
                        for h in &hashes {
                            info.known_blocks.insert(*h);
                        }
                    }
                    peer.send(&Message::Inv(items));
                }
                Ok(())
            }
            Message::Inv(items) => {
                self.on_inv(peer, items);
                Ok(())
            }
            Message::GetData(items) => self.on_getdata(peer, items),
            Message::NotFound(items) => {
                let mut info = peer.info();
                for item in items {
                    info.inflight.remove(&item.hash);
                }
                Ok(())
            }
            Message::Block(block) => {
                self.on_block(peer, *block);
                Ok(())
            }
            Message::Checkpoint(cp) => {
                let _ = self.process_checkpoint(*cp, Some(peer));
                Ok(())
            }
            Message::Revocation(rev) => {
                let _ = self.process_revocation(*rev, Some(peer));
                Ok(())
            }
            Message::Tx(tx) => {
                let txid = tx.txid();
                peer.info().known_txs.insert(txid);
                // Exactement la même transaction (signatures comprises) déjà
                // refusée depuis le dernier bloc : ignorée sans rien vérifier.
                if self.state().rejected.contains(&tx.wtxid()) {
                    return Ok(());
                }
                let _ = self.process_tx(*tx, Some(peer));
                Ok(())
            }
        }
    }

    fn on_version(&self, peer: &Arc<Peer>, v: VersionMessage) -> Result<(), String> {
        if peer.info().version.is_some() {
            return Err("message « version » en double".into());
        }
        if v.version < MIN_PROTOCOL_VERSION {
            return Err(format!("version de protocole {} trop ancienne", v.version));
        }
        if Some(v.genesis) != self.params().genesis.hash {
            return Err("pair d'un autre réseau (bloc de genèse différent)".into());
        }
        if self.inner.sent_nonces.lock().unwrap_or_else(|e| e.into_inner()).contains(&v.nonce) {
            // C'est le côté entrant qui le voit (la connexion sortante parle la
            // première) : on retrouve, par son nombre, la connexion sortante et
            // l'adresse qu'elle visait.
            let own_target = self.peer_list().iter().find(|p| p.nonce == v.nonce).and_then(|p| p.target);
            for t in peer.target.into_iter().chain(own_target) {
                self.addrbook().remove(&t);
                // Pair configuré (amorçage, --addnode) : ne plus le rejoindre.
                if self.inner.self_targets.lock().unwrap_or_else(|e| e.into_inner()).insert(t) {
                    info!("p2p", "{t} est ce nœud lui-même : adresse ignorée désormais");
                }
            }
            return Err("connexion à soi-même".into());
        }
        let listen_port = v.listen_port;
        {
            let mut info = peer.info();
            info.best_height = v.best_height;
            info.version = Some(v);
        }
        if peer.inbound {
            peer.send(&Message::Version(self.version_message(peer)));
            if listen_port != 0 {
                let ip = peer.addr.ip();
                self.addrbook().add_from(SocketAddr::new(ip, listen_port), ip);
            }
        }
        peer.send(&Message::Verack);
        Ok(())
    }

    fn on_verack(&self, peer: &Arc<Peer>) -> Result<(), String> {
        let (user_agent, height) = {
            let mut info = peer.info();
            let Some(version) = &info.version else {
                return Err("« verack » reçu avant « version »".into());
            };
            if info.got_verack {
                return Ok(());
            }
            let ua = version.user_agent.clone();
            info.got_verack = true;
            (ua, info.best_height)
        };
        info!(
            "p2p",
            "pair connecté : {} ({user_agent}, hauteur {height}, {})",
            peer.addr,
            if peer.inbound { "entrant" } else { "sortant" }
        );
        if !peer.inbound {
            peer.send(&Message::GetAddr);
            if let Some(t) = peer.target {
                self.addrbook().mark_success(&t);
                self.inner.retry.lock().unwrap_or_else(|e| e.into_inner()).remove(&t);
            }
        }
        self.request_blocks(peer, None, true);
        self.request_mempool_if_synced(peer);
        self.send_checkpoints_to(peer);
        Ok(())
    }

    /// Une fois à jour par rapport à un pair, lui demande ses transactions en
    /// attente (une seule fois par connexion). Avant d'être à jour, elles ne
    /// seraient pas vérifiables : une transaction soumise avant notre arrivée
    /// ne nous parviendrait alors jamais.
    fn request_mempool_if_synced(&self, peer: &Peer) {
        let height = self.state().chain.height();
        let send = {
            let mut info = peer.info();
            if info.mempool_requested || info.best_height > height {
                false
            } else {
                info.mempool_requested = true;
                true
            }
        };
        if send {
            peer.send(&Message::Mempool);
        }
    }

    /// Annonce des transactions à un pair, par lots. Sans `force`, celles que
    /// le pair connaît déjà sont sautées.
    fn announce_txs(&self, peer: &Peer, txids: &[Hash256], force: bool) {
        let fresh: Vec<InvItem> = {
            let mut info = peer.info();
            let list: Vec<Hash256> = txids.iter().copied().filter(|t| force || !info.known_txs.contains(t)).collect();
            for t in &list {
                info.known_txs.insert(*t);
            }
            list.into_iter().map(|hash| InvItem { kind: InvKind::Tx, hash }).collect()
        };
        for chunk in fresh.chunks(MAX_INV) {
            if !peer.send(&Message::Inv(chunk.to_vec())) {
                return;
            }
        }
    }

    /// Rediffuse les transactions locales encore en attente à tous les pairs
    /// (une annonce perdue, ou ignorée par un pair, ne doit pas laisser une
    /// transaction bloquée pour toujours) ; oublie celles qui ne sont plus en
    /// mempool (confirmées, remplacées ou évincées).
    fn rebroadcast_local_txs(&self) {
        let pending: Vec<Hash256> = {
            let st = self.state();
            let mut local = self.inner.local_txs.lock().unwrap_or_else(|e| e.into_inner());
            local.retain(|t| st.mempool.contains(t));
            local.iter().copied().collect()
        };
        if pending.is_empty() {
            return;
        }
        debug!("mempool", "rediffusion de {} transaction(s) locale(s)", pending.len());
        for peer in self.peer_list() {
            if peer.info().handshake_done() {
                self.announce_txs(&peer, &pending, true);
            }
        }
    }

    fn on_getaddr(&self, peer: &Peer) {
        {
            let mut info = peer.info();
            if info.answered_getaddr {
                return;
            }
            info.answered_getaddr = true;
        }
        let addrs = self.addrbook().sample(MAX_ADDR);
        if !addrs.is_empty() {
            peer.send(&Message::Addr(addrs));
        }
    }

    /// Demande à un pair les blocs qui suivent notre chaîne (ou `from`).
    /// Sans `force`, seulement si le pair semble plus avancé et que rien
    /// n'est en cours.
    pub(crate) fn request_blocks(&self, peer: &Peer, from: Option<Hash256>, force: bool) {
        let (height, locator) = {
            let st = self.state();
            let locator = match from {
                Some(h) if st.chain.entry(&h).is_some() => st.chain.locator_from(&h),
                _ => st.chain.locator(),
            };
            (st.chain.height(), locator)
        };
        {
            let mut info = peer.info();
            if !force {
                if info.best_height <= height || !info.inflight.is_empty() {
                    return;
                }
                if info.last_getblocks.is_some_and(|t| t.elapsed() < Duration::from_secs(5)) {
                    return;
                }
            }
            info.last_getblocks = Some(Instant::now());
        }
        peer.send(&Message::GetBlocks { locator, stop: Hash256::ZERO });
    }

    fn on_inv(&self, peer: &Arc<Peer>, items: Vec<InvItem>) {
        let mut want_blocks = Vec::new();
        let mut want_txs = Vec::new();
        {
            let st = self.state();
            for item in &items {
                match item.kind {
                    InvKind::Block => {
                        if !st.chain.contains(&item.hash) {
                            want_blocks.push(item.hash);
                        }
                    }
                    // Pas de filtre sur les refus récents ici : ils sont indexés
                    // par wtxid, que l'annonce ne porte pas. Une copie altérée
                    // envoyée par un tiers ne doit pas faire ignorer la vraie.
                    InvKind::Tx => {
                        if !st.mempool.contains(&item.hash) && st.chain.tx_height(&item.hash).is_none() {
                            want_txs.push(item.hash);
                        }
                    }
                }
            }
        }
        // Un bloc déjà en cours de téléchargement auprès d'un autre pair n'est
        // pas redemandé (sinon la synchronisation télécharge tout plusieurs fois).
        let elsewhere: HashSet<Hash256> = if want_blocks.is_empty() {
            HashSet::new()
        } else {
            let now = Instant::now();
            self.peer_list()
                .iter()
                .filter(|p| p.id != peer.id)
                .flat_map(|p| {
                    p.info()
                        .inflight
                        .iter()
                        .filter(|(_, since)| now.duration_since(**since) < BLOCK_DOWNLOAD_TIMEOUT)
                        .map(|(h, _)| *h)
                        .collect::<Vec<_>>()
                })
                .collect()
        };
        let block_count = items.iter().filter(|i| i.kind == InvKind::Block).count();
        let last_block = items.iter().rev().find(|i| i.kind == InvKind::Block).map(|i| i.hash);
        let full_batch = block_count >= GETBLOCKS_BATCH;
        let mut request = Vec::new();
        {
            let mut info = peer.info();
            for item in &items {
                match item.kind {
                    InvKind::Block => info.known_blocks.insert(item.hash),
                    InvKind::Tx => info.known_txs.insert(item.hash),
                }
            }
            if full_batch {
                info.sync_continue_from = last_block;
            }
            let room = MAX_INFLIGHT.saturating_sub(info.inflight.len());
            let now = Instant::now();
            for h in want_blocks.iter().filter(|h| !elsewhere.contains(h)).take(room) {
                info.inflight.insert(*h, now);
                request.push(InvItem { kind: InvKind::Block, hash: *h });
            }
            for h in want_txs.iter().take(MAX_INV.saturating_sub(request.len())) {
                request.push(InvItem { kind: InvKind::Tx, hash: *h });
            }
        }
        if !request.is_empty() {
            peer.send(&Message::GetData(request));
        }
        if full_batch && want_blocks.is_empty() {
            // Lot déjà connu : on demande directement la suite.
            peer.info().sync_continue_from = None;
            self.request_blocks(peer, last_block, true);
        }
    }

    /// Contre-pression : attend que le pair ait absorbé ce qui est déjà en
    /// file avant de lui envoyer un gros objet. `Err` : pair trop lent.
    fn wait_for_send_room(&self, peer: &Peer) -> Result<(), String> {
        let waiting_since = Instant::now();
        while peer.queued_bytes() > MAX_QUEUED_BYTES {
            if self.is_shutting_down() || waiting_since.elapsed() > Duration::from_secs(60) {
                return Err("pair trop lent à recevoir les données demandées".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }

    fn on_getdata(&self, peer: &Arc<Peer>, items: Vec<InvItem>) -> Result<(), String> {
        enum Source {
            Memory(Box<Block>),
            Disk(crate::storage::BlockLocation),
            Missing,
        }
        let mut not_found = Vec::new();
        let mut seen = HashSet::with_capacity(items.len());
        for item in items {
            // Un même objet demandé plusieurs fois n'est envoyé qu'une fois.
            if !seen.insert(item) {
                continue;
            }
            match item.kind {
                InvKind::Block => {
                    // Sous le verrou : seulement l'emplacement du bloc. La lecture
                    // du disque se fait ensuite, sans bloquer le reste du nœud.
                    let source = {
                        let st = self.state();
                        if st.chain.entry(&item.hash).is_none() {
                            Source::Missing
                        } else if st.chain.is_parked(&item.hash) {
                            match st.chain.get_block(&item.hash) {
                                Ok(Some(block)) => Source::Memory(Box::new(block)),
                                _ => Source::Missing,
                            }
                        } else {
                            st.chain.store().location(&item.hash).map_or(Source::Missing, Source::Disk)
                        }
                    };
                    let frame = match source {
                        Source::Memory(block) => Some(Message::Block(block).to_frame(self.params().magic)),
                        Source::Disk(location) => match location.read() {
                            Ok(bytes) => Some(block_frame(self.params().magic, &bytes)),
                            Err(e) => {
                                crate::error!("stockage", "lecture du bloc {} impossible : {e}", item.hash);
                                None
                            }
                        },
                        Source::Missing => None,
                    };
                    match frame {
                        Some(frame) => {
                            self.wait_for_send_room(peer)?;
                            peer.info().known_blocks.insert(item.hash);
                            if !peer.send_frame(frame) {
                                return Err("file d'envoi pleine".into());
                            }
                        }
                        None => not_found.push(item),
                    }
                }
                InvKind::Tx => {
                    let tx = self.state().mempool.get(&item.hash).map(|e| e.tx.clone());
                    match tx {
                        Some(tx) => {
                            self.wait_for_send_room(peer)?;
                            if !peer.send(&Message::Tx(Box::new(tx))) {
                                return Err("file d'envoi pleine".into());
                            }
                        }
                        None => not_found.push(item),
                    }
                }
            }
        }
        if !not_found.is_empty() {
            peer.send(&Message::NotFound(not_found));
        }
        Ok(())
    }

    fn on_block(&self, peer: &Arc<Peer>, block: Block) {
        let hash = block.hash();
        let continue_sync = {
            let mut info = peer.info();
            info.inflight.remove(&hash);
            info.known_blocks.insert(hash);
            if info.sync_continue_from == Some(hash) {
                info.sync_continue_from = None;
                true
            } else {
                false
            }
        };
        let result = self.process_block(block, Some(peer));
        if let Some(height) = self.state().chain.entry(&hash).map(|e| e.height) {
            let mut info = peer.info();
            info.best_height = info.best_height.max(height);
        }
        match result {
            Ok(BlockResult::Orphan) => self.request_blocks(peer, None, true),
            _ if continue_sync => self.request_blocks(peer, Some(hash), true),
            _ => self.request_blocks(peer, None, false),
        }
        self.request_mempool_if_synced(peer);
    }

    /// Soumet un bloc (mineur local, outils, tests).
    pub fn submit_block(&self, block: Block) -> Result<BlockResult, BlockError> {
        self.process_block(block, None)
    }

    /// Soumet une transaction locale (RPC, tests) : validation puis relais.
    pub fn submit_tx(&self, tx: Transaction) -> Result<Hash256, MempoolError> {
        self.process_tx(tx, None)
    }

    /// Traite un bloc venant d'un pair (`source`) ou du mineur local (`None`).
    pub(crate) fn process_block(&self, block: Block, source: Option<&Arc<Peer>>) -> Result<BlockResult, BlockError> {
        let hash = block.hash();
        let now = now_secs();
        let (result, announce, height, tip, fatal) = {
            let mut st = self.state();
            let result = st.chain.process_block(block, now);
            let mut announce = Vec::new();
            if let Ok(ProcessOutcome::Accepted { tip_change: Some(tc), .. }) = &result {
                apply_tip_change(&mut st, tc);
                // Nouvel état : un refus de politique (frais, place) peut ne plus tenir.
                st.rejected.clear();
                announce = if tc.connected.len() <= 10 {
                    tc.connected.iter().map(Block::hash).collect()
                } else {
                    vec![st.chain.tip_hash()]
                };
            }
            let fatal = st.chain.fatal_error().map(str::to_string);
            (result, announce, st.chain.height(), st.chain.tip_hash(), fatal)
        };
        if let Some(message) = fatal {
            // Le stockage ne permet plus de suivre la chaîne : arrêt propre. Au
            // redémarrage, l'état est reconstruit depuis le disque (ou le
            // problème est signalé clairement s'il persiste).
            crate::error!("stockage", "erreur de stockage grave, arrêt du nœud : {message}");
            self.shutdown();
        }
        match result {
            Ok(ProcessOutcome::Accepted { tip_change, invalid }) => {
                let mut own_error = None;
                for (h, e) in invalid {
                    if h == hash {
                        own_error = Some(e);
                    } else {
                        warn!("chaîne", "bloc {h} invalide : {e}");
                    }
                }
                if tip_change.is_some() {
                    self.inner.miner.bump();
                    self.relay_blocks(&announce, source);
                    info!("chaîne", "nouveau sommet : hauteur {height} ({tip})");
                    self.sign_tip();
                }
                self.retry_pending_checkpoint();
                match own_error {
                    Some(e) => {
                        // Une erreur de stockage n'est pas la faute du pair.
                        if let Some(p) = source
                            && e.is_peer_fault()
                        {
                            self.punish(p, BAN_THRESHOLD, &format!("bloc invalide : {e}"));
                        }
                        Err(e)
                    }
                    None if tip == hash || tip_change.is_some() => Ok(BlockResult::NewTip),
                    None => Ok(BlockResult::SideChain),
                }
            }
            Ok(ProcessOutcome::Orphan { missing_parent }) => {
                debug!("chaîne", "bloc orphelin {hash} (parent {missing_parent} inconnu)");
                Ok(BlockResult::Orphan)
            }
            Ok(ProcessOutcome::AlreadyKnown) => Ok(BlockResult::AlreadyKnown),
            Err(e) => {
                match source {
                    Some(p) if e.is_peer_fault() => self.punish(p, BAN_THRESHOLD, &format!("bloc refusé : {e}")),
                    _ => debug!("chaîne", "bloc {hash} refusé : {e}"),
                }
                Err(e)
            }
        }
    }

    /// Valide une transaction et la relaie si elle entre dans la mempool.
    ///
    /// Les vérifications bon marché (format, doublon, pièces, frais, place)
    /// passent d'abord, sous le verrou principal. Les signatures, coûteuses,
    /// sont vérifiées ensuite **hors du verrou** : le reste du nœud continue
    /// pendant ce temps. Puis, sous le verrou, tout est revérifié (l'état a pu
    /// changer) et la transaction est insérée.
    pub(crate) fn process_tx(&self, tx: Transaction, source: Option<&Arc<Peer>>) -> Result<Hash256, MempoolError> {
        let txid = tx.txid();
        let wtxid = tx.wtxid();
        // Alerte de points de contrôle : aucun paiement ne doit pouvoir être
        // considéré comme reçu tant que la clé maîtresse n'a pas tranché.
        if self.checkpoints().alert.is_some() {
            return Err(MempoolError::Suspended);
        }
        let (first, genesis) = {
            let st = self.state();
            (st.mempool.precheck(&tx, st.chain.utxo(), st.chain.height() + 1, self.params()), st.chain.genesis_hash())
        };
        let result = match first {
            Err(e) => Err(e),
            Ok(checked) => match verify_input_signatures(&tx, &checked.spent, &genesis) {
                Err(e) => Err(MempoolError::Invalid(e)),
                Ok(()) => {
                    let mut guard = self.state();
                    let st = &mut *guard;
                    match st.mempool.precheck(&tx, st.chain.utxo(), st.chain.height() + 1, self.params()) {
                        Err(e) => Err(e),
                        // Une référence « txid:index » désigne toujours la même sortie :
                        // les sorties dépensées, donc les signatures, n'ont pas pu changer.
                        // Vérification par prudence malgré tout.
                        Ok(again) if again.spent != checked.spent => {
                            match verify_input_signatures(&tx, &again.spent, &genesis) {
                                Ok(()) => Ok(st.mempool.insert_checked(tx, again)),
                                Err(e) => Err(MempoolError::Invalid(e)),
                            }
                        }
                        Ok(again) => Ok(st.mempool.insert_checked(tx, again)),
                    }
                }
            },
        };
        if let Err(e) = &result {
            let may_become_valid = matches!(
                e,
                MempoolError::AlreadyKnown
                    | MempoolError::Conflict(_)
                    | MempoolError::Invalid(TxError::MissingInput(_) | TxError::ImmatureCoinbase { .. })
            );
            if !may_become_valid {
                self.state().rejected.insert(wtxid);
            }
        }
        match &result {
            Ok(_) => {
                debug!("mempool", "transaction acceptée {txid}");
                if source.is_none() {
                    self.inner.local_txs.lock().unwrap_or_else(|e| e.into_inner()).insert(txid);
                }
                self.relay_tx(txid, source);
                self.inner.miner.bump_mempool();
            }
            Err(e) => {
                if let Some(p) = source
                    && e.is_invalid_everywhere()
                {
                    self.punish(p, BAN_THRESHOLD, &format!("transaction invalide : {e}"));
                }
            }
        }
        result
    }

    fn relay_blocks(&self, hashes: &[Hash256], except: Option<&Arc<Peer>>) {
        if hashes.is_empty() {
            return;
        }
        for peer in self.peer_list() {
            if except.is_some_and(|e| e.id == peer.id) {
                continue;
            }
            let items: Vec<InvItem> = {
                let mut info = peer.info();
                if !info.handshake_done() {
                    continue;
                }
                let fresh: Vec<Hash256> = hashes.iter().copied().filter(|h| !info.known_blocks.contains(h)).collect();
                for h in &fresh {
                    info.known_blocks.insert(*h);
                }
                fresh.into_iter().map(|hash| InvItem { kind: InvKind::Block, hash }).collect()
            };
            if !items.is_empty() {
                peer.send(&Message::Inv(items));
            }
        }
    }

    fn relay_tx(&self, txid: Hash256, except: Option<&Arc<Peer>>) {
        for peer in self.peer_list() {
            if except.is_some_and(|e| e.id == peer.id) {
                continue;
            }
            {
                let mut info = peer.info();
                if !info.handshake_done() || info.known_txs.contains(&txid) {
                    continue;
                }
                info.known_txs.insert(txid);
            }
            peer.send(&Message::Inv(vec![InvItem { kind: InvKind::Tx, hash: txid }]));
        }
    }

    /// Tâches périodiques de synchronisation.
    fn sync_tick(&self) {
        for peer in self.peer_list() {
            if peer.info().handshake_done() {
                self.request_blocks(&peer, None, false);
            }
        }
    }

    fn is_connected_to(&self, addr: &SocketAddr) -> bool {
        self.peer_list().iter().any(|p| p.target == Some(*addr) || p.addr == *addr)
    }

    fn spawn_connect(&self, addr: SocketAddr) {
        {
            let mut connecting = self.inner.connecting.lock().unwrap_or_else(|e| e.into_inner());
            if !connecting.insert(addr) {
                return;
            }
        }
        self.addrbook().mark_attempt(&addr);
        let node = self.clone();
        let spawned = std::thread::Builder::new().name(format!("connexion-{addr}")).spawn(move || {
            let result = TcpStream::connect_timeout(&addr, Duration::from_secs(5));
            node.inner.connecting.lock().unwrap_or_else(|e| e.into_inner()).remove(&addr);
            match result {
                Ok(stream) => run_peer(node, stream, false, Some(addr)),
                Err(e) => {
                    debug!("p2p", "connexion à {addr} impossible : {e}");
                    node.addrbook().mark_failure(&addr);
                    let mut retry = node.inner.retry.lock().unwrap_or_else(|e| e.into_inner());
                    let failures = retry.get(&addr).map_or(0, |(_, f)| *f).saturating_add(1);
                    let delay = Duration::from_secs((1u64 << failures.min(6)).min(60));
                    retry.insert(addr, (Instant::now() + delay, failures));
                }
            }
        });
        if spawned.is_err() {
            self.inner.connecting.lock().unwrap_or_else(|e| e.into_inner()).remove(&addr);
        }
    }

    fn connection_tick(&self) {
        // Bannissements expirés.
        self.inner.bans.lock().unwrap_or_else(|e| e.into_inner()).retain(|_, until| Instant::now() < *until);

        let config = &self.inner.config;
        let mut targets: Vec<SocketAddr> = config.connect.clone();
        targets.extend(self.inner.extra_peers.lock().unwrap_or_else(|e| e.into_inner()).iter().copied());
        let ignored = self.ignored_peers();
        targets.retain(|t| !ignored.contains(t));
        for t in targets {
            if self.is_connected_to(&t) || self.inner.connecting.lock().unwrap_or_else(|e| e.into_inner()).contains(&t)
            {
                continue;
            }
            let due = self
                .inner
                .retry
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&t)
                .is_none_or(|(next, _)| Instant::now() >= *next);
            if due {
                self.spawn_connect(t);
            }
        }

        // Découverte : compléter les connexions sortantes avec le carnet d'adresses.
        if config.connect.is_empty() {
            let peers = self.peer_list();
            let outbound = peers.iter().filter(|p| !p.inbound).count()
                + self.inner.connecting.lock().unwrap_or_else(|e| e.into_inner()).len();
            if outbound < config.max_outbound {
                let mut exclude: HashSet<SocketAddr> = peers.iter().map(|p| p.addr).collect();
                exclude.extend(peers.iter().filter_map(|p| p.target));
                if let Some(own) = self.inner.p2p_addr {
                    exclude.insert(own);
                }
                // Diversité : au plus un pair sortant par groupe d'adresses (/16
                // en IPv4, /32 en IPv6) : un attaquant contrôle rarement des
                // adresses très dispersées.
                let groups: HashSet<(u8, u32)> =
                    peers.iter().filter(|p| !p.inbound).filter_map(|p| netgroup(&p.addr.ip())).collect();
                for _ in 0..10 {
                    let candidate = self.addrbook().pick(&exclude);
                    let Some(addr) = candidate else { break };
                    exclude.insert(addr);
                    let same_group = netgroup(&addr.ip()).is_some_and(|g| groups.contains(&g));
                    if is_usable(&addr) && !self.is_banned(&addr.ip()) && !same_group {
                        self.spawn_connect(addr);
                        break;
                    }
                }
            }
        }
    }
}

/// Groupe d'une adresse routable : (famille, /16 en IPv4 ou /32 en IPv6).
/// None pour les adresses locales ou privées, qui ne sont pas restreintes.
fn netgroup(ip: &IpAddr) -> Option<(u8, u32)> {
    if !crate::p2p::addrbook::is_routable(ip) {
        return None;
    }
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            Some((4, u32::from(o[0]) << 8 | u32::from(o[1])))
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            Some((6, u32::from(s[0]) << 16 | u32::from(s[1])))
        }
    }
}

/// Met la mempool à jour après un changement de chaîne active.
pub(crate) fn apply_tip_change(st: &mut State, tc: &TipChange) {
    for block in &tc.connected {
        st.mempool.remove_for_block(block);
    }
    let next_height = st.chain.height() + 1;
    let genesis = st.chain.genesis_hash();
    let params = st.chain.params();
    for block in &tc.disconnected {
        for tx in block.transactions.iter().skip(1) {
            let _ = st.mempool.accept(tx.clone(), st.chain.utxo(), next_height, params, &genesis);
        }
    }
    st.mempool.revalidate(st.chain.utxo(), next_height, params);
}

fn accept_loop(node: &Node, listener: &TcpListener) {
    if let Err(e) = listener.set_nonblocking(true) {
        crate::error!("p2p", "configuration de l'écoute impossible : {e}");
        return;
    }
    while !node.is_shutting_down() {
        match listener.accept() {
            Ok((stream, addr)) => {
                if node.is_banned(&addr.ip()) {
                    debug!("p2p", "connexion refusée depuis {addr} (banni)");
                    continue;
                }
                let n = node.clone();
                let spawned = std::thread::Builder::new()
                    .name(format!("pair-entrant-{addr}"))
                    .spawn(move || run_peer(n, stream, true, None));
                if let Err(e) = spawned {
                    warn!("p2p", "impossible de gérer la connexion de {addr} : {e}");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                debug!("p2p", "erreur d'acceptation : {e}");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn connection_manager(node: &Node) {
    let mut last_sync = Instant::now();
    let mut last_save = Instant::now();
    let mut last_rebroadcast = Instant::now();
    while !node.is_shutting_down() {
        std::thread::sleep(Duration::from_millis(500));
        node.connection_tick();
        if last_sync.elapsed() > Duration::from_secs(10) {
            node.sync_tick();
            last_sync = Instant::now();
        }
        if last_rebroadcast.elapsed() > REBROADCAST_INTERVAL {
            node.rebroadcast_local_txs();
            last_rebroadcast = Instant::now();
        }
        if last_save.elapsed() > Duration::from_secs(300) {
            node.addrbook().save(&node.inner.dir.join("peers.txt"));
            last_save = Instant::now();
        }
    }
}
