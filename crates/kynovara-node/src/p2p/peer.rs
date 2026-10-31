//! Connexion avec un pair : un fil de lecture (protocole) et un fil
//! d'écriture (file d'attente des messages sortants).

use super::message::{FrameReader, Message, VersionMessage};
use crate::node::Node;
use kynovara_core::hash::Hash256;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;
use std::io::{ErrorKind, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Messages en attente d'envoi au maximum (au-delà : pair trop lent, déconnecté).
const SEND_QUEUE: usize = 4096;
/// Octets en attente d'envoi au-delà desquels on attend avant de servir de
/// gros objets (blocs, transactions) à un pair.
pub(crate) const MAX_QUEUED_BYTES: usize = 32 * 1024 * 1024;
/// Plafond absolu, pour tout type de message : un pair qui ne lit plus et
/// laisserait dépasser ce volume est déconnecté (mémoire bornée par pair).
const HARD_QUEUE_LIMIT: usize = 2 * MAX_QUEUED_BYTES;
/// Délai de lecture : le fil de lecture vérifie ses minuteries à ce rythme.
const READ_POLL: Duration = Duration::from_millis(250);
pub(crate) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
pub(crate) const PING_INTERVAL: Duration = Duration::from_secs(60);
pub(crate) const PING_TIMEOUT: Duration = Duration::from_secs(90);
pub(crate) const INACTIVITY_TIMEOUT: Duration = Duration::from_secs(600);
pub(crate) const BLOCK_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
/// Score de mauvais comportement entraînant le bannissement.
pub(crate) const BAN_THRESHOLD: u32 = 100;
/// Jetons de débit : capacité et recharge par seconde (messages « bon marché »).
const TOKEN_CAPACITY: f64 = 2_000.0;
const TOKEN_REFILL_PER_SEC: f64 = 500.0;

/// Ensemble borné (les plus anciens éléments sont oubliés).
#[derive(Debug)]
pub(crate) struct BoundedSet<T: Eq + Hash + Copy> {
    set: HashSet<T>,
    order: VecDeque<T>,
    cap: usize,
}

impl<T: Eq + Hash + Copy> BoundedSet<T> {
    pub(crate) fn new(cap: usize) -> Self {
        BoundedSet { set: HashSet::new(), order: VecDeque::new(), cap }
    }

    pub(crate) fn insert(&mut self, v: T) {
        if self.set.insert(v) {
            self.order.push_back(v);
            while self.order.len() > self.cap {
                if let Some(old) = self.order.pop_front() {
                    self.set.remove(&old);
                }
            }
        }
    }

    pub(crate) fn contains(&self, v: &T) -> bool {
        self.set.contains(v)
    }

    pub(crate) fn clear(&mut self) {
        self.set.clear();
        self.order.clear();
    }
}

/// État d'un pair (protégé par un verrou).
#[derive(Debug)]
pub(crate) struct PeerInfo {
    pub version: Option<VersionMessage>,
    pub got_verack: bool,
    pub best_height: u64,
    pub misbehavior: u32,
    pub last_recv: Instant,
    pub ping: Option<(u64, Instant)>,
    pub last_ping: Instant,
    /// Blocs demandés et pas encore reçus.
    pub inflight: HashMap<Hash256, Instant>,
    /// Dernier bloc d'un lot de synchronisation : à sa réception, on demande la suite.
    pub sync_continue_from: Option<Hash256>,
    pub last_getblocks: Option<Instant>,
    pub answered_getaddr: bool,
    /// Demande `mempool` envoyée à ce pair (une fois synchronisé avec lui).
    pub mempool_requested: bool,
    /// Demande `mempool` de ce pair déjà servie (une fois par connexion).
    pub answered_mempool: bool,
    /// Nombre d'adresses reçues sur cette connexion (la limite par source,
    /// toutes connexions confondues, est tenue par le carnet d'adresses).
    pub addr_received: usize,
    pub known_blocks: BoundedSet<Hash256>,
    pub known_txs: BoundedSet<Hash256>,
    tokens: f64,
    last_refill: Instant,
    pub bytes_received: u64,
}

impl PeerInfo {
    fn new() -> Self {
        let now = Instant::now();
        PeerInfo {
            version: None,
            got_verack: false,
            best_height: 0,
            misbehavior: 0,
            last_recv: now,
            ping: None,
            last_ping: now,
            inflight: HashMap::new(),
            sync_continue_from: None,
            last_getblocks: None,
            answered_getaddr: false,
            mempool_requested: false,
            answered_mempool: false,
            addr_received: 0,
            known_blocks: BoundedSet::new(5_000),
            known_txs: BoundedSet::new(50_000),
            tokens: TOKEN_CAPACITY,
            last_refill: now,
            bytes_received: 0,
        }
    }

    /// Présentation terminée (version reçue et confirmée).
    pub fn handshake_done(&self) -> bool {
        self.version.is_some() && self.got_verack
    }

    /// Consomme un jeton de débit ; `false` si le pair envoie trop de messages.
    fn take_token(&mut self, cost: f64) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.last_refill = now;
        self.tokens = (self.tokens + elapsed * TOKEN_REFILL_PER_SEC).min(TOKEN_CAPACITY);
        if self.tokens < cost {
            return false;
        }
        self.tokens -= cost;
        true
    }
}

/// Un pair connecté.
#[derive(Debug)]
pub(crate) struct Peer {
    pub id: u64,
    pub addr: SocketAddr,
    pub inbound: bool,
    /// Adresse configurée (connexion sortante).
    pub target: Option<SocketAddr>,
    pub connected_at: Instant,
    /// Nombre aléatoire propre à cette connexion, envoyé dans notre message
    /// `version` (détection des connexions à soi-même ; un nombre unique par
    /// connexion évite de pouvoir relier nos connexions entre elles).
    pub nonce: u64,
    magic: [u8; 4],
    sender: SyncSender<Vec<u8>>,
    queued: Arc<AtomicUsize>,
    stream: TcpStream,
    info: Mutex<PeerInfo>,
}

impl Peer {
    /// Accès à l'état du pair (un verrou empoisonné n'est pas fatal ici).
    pub fn info(&self) -> MutexGuard<'_, PeerInfo> {
        self.info.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Met un message en file d'envoi. Un pair dont la file déborde (en
    /// nombre de messages ou en octets) est déconnecté.
    pub fn send(&self, msg: &Message) -> bool {
        self.send_frame(msg.to_frame(self.magic))
    }

    /// Met une trame déjà construite en file d'envoi (voir [`send`](Self::send)).
    pub fn send_frame(&self, frame: Vec<u8>) -> bool {
        let len = frame.len();
        let before = self.queued.fetch_add(len, Ordering::SeqCst);
        if before + len > HARD_QUEUE_LIMIT || self.sender.try_send(frame).is_err() {
            self.queued.fetch_sub(len, Ordering::SeqCst);
            self.disconnect();
            return false;
        }
        true
    }

    /// Octets en attente d'envoi.
    pub fn queued_bytes(&self) -> usize {
        self.queued.load(Ordering::SeqCst)
    }

    pub fn disconnect(&self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

fn writer_loop(mut stream: TcpStream, rx: Receiver<Vec<u8>>, queued: Arc<AtomicUsize>) {
    for frame in rx {
        let result = stream.write_all(&frame);
        queued.fetch_sub(frame.len(), Ordering::SeqCst);
        if result.is_err() {
            let _ = stream.shutdown(Shutdown::Both);
            break;
        }
    }
}

/// Gère une connexion jusqu'à sa fermeture (appelé dans un fil dédié).
pub(crate) fn run_peer(node: Node, stream: TcpStream, inbound: bool, target: Option<SocketAddr>) {
    let Ok(addr) = stream.peer_addr() else {
        return;
    };
    let setup = stream
        .set_nonblocking(false)
        .and_then(|()| stream.set_nodelay(true))
        .and_then(|()| stream.set_read_timeout(Some(READ_POLL)))
        .and_then(|()| stream.set_write_timeout(Some(Duration::from_secs(30))));
    let (Ok(()), Ok(write_stream), Ok(control_stream)) = (setup, stream.try_clone(), stream.try_clone()) else {
        return;
    };
    let (tx, rx) = sync_channel(SEND_QUEUE);
    let queued = Arc::new(AtomicUsize::new(0));
    let peer = Arc::new(Peer {
        id: node.next_peer_id(),
        addr,
        inbound,
        target,
        connected_at: Instant::now(),
        nonce: crate::node::random_u64(),
        magic: node.params().magic,
        sender: tx,
        queued: Arc::clone(&queued),
        stream: control_stream,
        info: Mutex::new(PeerInfo::new()),
    });
    if !node.register_peer(&peer) {
        peer.disconnect();
        return;
    }
    let spawned = std::thread::Builder::new()
        .name(format!("pair-{}-envoi", peer.id))
        .spawn(move || writer_loop(write_stream, rx, queued));
    if spawned.is_err() {
        node.unregister_peer(peer.id);
        peer.disconnect();
        return;
    }
    let reason = peer_loop(&node, &peer, stream);
    let shown = peer.info().handshake_done();
    node.unregister_peer(peer.id);
    peer.disconnect();
    if shown {
        crate::info!("p2p", "pair {} déconnecté : {reason}", peer.addr);
    } else {
        crate::debug!("p2p", "connexion {} fermée : {reason}", peer.addr);
    }
}

fn peer_loop(node: &Node, peer: &Arc<Peer>, mut stream: TcpStream) -> String {
    let mut reader = FrameReader::new(node.params().magic);
    if !peer.inbound {
        peer.send(&Message::Version(node.version_message(peer)));
    }
    loop {
        if node.is_shutting_down() {
            return "arrêt du nœud".into();
        }
        if let Some(reason) = node.check_peer_timers(peer) {
            return reason;
        }
        match reader.read_from(&mut stream) {
            Ok(0) => return "connexion fermée par le pair".into(),
            Ok(n) => {
                let mut info = peer.info();
                info.bytes_received += n as u64;
                info.last_recv = Instant::now();
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => {
                continue;
            }
            Err(e) => return format!("erreur réseau : {e}"),
        }
        loop {
            let (command, payload) = match reader.next_frame() {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                Err(e) => {
                    node.punish(peer, BAN_THRESHOLD, &format!("violation du protocole : {e}"));
                    return format!("violation du protocole : {e}");
                }
            };
            let msg = match Message::decode(command, &payload) {
                Ok(m) => m,
                Err(e) => {
                    node.punish(peer, BAN_THRESHOLD, &format!("message illisible : {e}"));
                    return format!("message illisible : {e}");
                }
            };
            // Débit limité : un bloc demandé est gratuit (synchronisation), un
            // bloc non sollicité coûte cher, une transaction coûte selon son
            // nombre d'entrées (autant de signatures à vérifier), tout autre
            // message coûte 1 jeton.
            let cost = match &msg {
                Message::Block(b) if peer.info().inflight.contains_key(&b.hash()) => 0.0,
                Message::Block(_) => 10.0,
                Message::Tx(tx) => 1.0 + tx.inputs().len() as f64 / 4.0,
                // Deux signatures à vérifier.
                Message::Checkpoint(_) | Message::Revocation(_) => 4.0,
                _ => 1.0,
            };
            if !peer.info().take_token(cost) {
                node.punish(peer, BAN_THRESHOLD, "trop de messages");
                return "trop de messages".into();
            }
            crate::debug!("p2p", "{} -> {}", peer.addr, msg.name());
            if let Err(reason) = node.handle_message(peer, msg) {
                return reason;
            }
            if peer.info().misbehavior >= BAN_THRESHOLD {
                return "comportement abusif".into();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_set_forgets_oldest() {
        let mut s = BoundedSet::new(3);
        for i in 0..5 {
            s.insert(i);
        }
        assert!(!s.contains(&0) && !s.contains(&1));
        assert!(s.contains(&2) && s.contains(&4));
        s.insert(4);
        assert!(s.contains(&2));
    }

    #[test]
    fn token_bucket_limits_bursts() {
        let mut info = PeerInfo::new();
        let mut accepted = 0;
        for _ in 0..5_000 {
            if info.take_token(1.0) {
                accepted += 1;
            }
        }
        assert!((2_000..2_100).contains(&accepted), "{accepted}");
        assert!(info.take_token(0.0));
    }
}
