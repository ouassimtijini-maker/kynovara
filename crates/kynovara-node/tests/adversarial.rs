//! Tests adverses : un faux pair malveillant écrit directement sur la socket.
#![allow(clippy::unwrap_used)]

use kynovara_core::block::Block;
use kynovara_core::genesis::genesis_block;
use kynovara_core::hash::Hash256;
use kynovara_core::params::Network;
use kynovara_node::logger::Level;
use kynovara_node::p2p::message::{FrameReader, InvItem, InvKind, Message, VersionMessage};
use kynovara_node::{Node, NodeConfig};
use std::io::{ErrorKind, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn temp_dir(name: &str) -> PathBuf {
    let mut b = [0u8; 8];
    getrandom::getrandom(&mut b).unwrap();
    let dir = std::env::temp_dir().join(format!("kynovara-adv-{name}-{:x}", u64::from_le_bytes(b)));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn config(dir: &Path) -> NodeConfig {
    let mut c = NodeConfig::default_for(Network::Regtest);
    c.datadir = dir.to_path_buf();
    c.listen = Some("127.0.0.1:0".parse().unwrap());
    c.rpc = "127.0.0.1:0".parse().unwrap();
    c.log_level = Level::Error;
    c
}

fn start(dir: &Path) -> Node {
    Node::start(config(dir)).unwrap()
}

fn magic() -> [u8; 4] {
    Network::Regtest.params().magic
}

struct FakePeer {
    stream: TcpStream,
    reader: FrameReader,
}

impl FakePeer {
    fn connect(node: &Node) -> FakePeer {
        let stream = TcpStream::connect(node.p2p_addr().unwrap()).unwrap();
        stream.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        FakePeer { stream, reader: FrameReader::new(magic()) }
    }

    fn send(&mut self, msg: &Message) {
        let _ = self.stream.write_all(&msg.to_frame(magic()));
    }

    fn version(genesis: Hash256) -> Message {
        Message::Version(VersionMessage {
            version: 1,
            genesis,
            best_height: 0,
            nonce: 42,
            listen_port: 0,
            timestamp: 0,
            user_agent: "/faux:1/".into(),
        })
    }

    /// Attend un message (ou None si la connexion est fermée / délai dépassé).
    fn recv(&mut self, timeout: Duration) -> Option<Message> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(Some((cmd, payload))) = self.reader.next_frame() {
                return Message::decode(cmd, &payload).ok();
            }
            if Instant::now() > deadline {
                return None;
            }
            match self.reader.read_from(&mut self.stream) {
                Ok(0) => return None,
                Ok(_) => {}
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(_) => return None,
            }
        }
    }

    /// La connexion a-t-elle été fermée par le nœud ?
    fn is_closed(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match self.reader.read_from(&mut self.stream) {
                Ok(0) => return true,
                Ok(_) => {}
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(_) => return true,
            }
        }
        false
    }

    /// Présentation complète.
    fn handshake(&mut self) {
        let genesis = Network::Regtest.params().genesis.hash.unwrap();
        self.send(&FakePeer::version(genesis));
        let mut got_version = false;
        let mut got_verack = false;
        while !(got_version && got_verack) {
            match self.recv(Duration::from_secs(5)) {
                Some(Message::Version(_)) => got_version = true,
                Some(Message::Verack) => got_verack = true,
                Some(_) => {}
                None => panic!("présentation interrompue"),
            }
        }
        self.send(&Message::Verack);
    }
}

fn stop(node: Node, dir: PathBuf) {
    node.shutdown();
    node.wait();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn wrong_network_is_disconnected() {
    let dir = temp_dir("net");
    let node = start(&dir);
    let mut peer = FakePeer::connect(&node);
    peer.send(&FakePeer::version(Hash256([7; 32])));
    assert!(peer.is_closed(Duration::from_secs(5)));
    stop(node, dir);
}

#[test]
fn message_before_handshake_is_refused() {
    let dir = temp_dir("early");
    let node = start(&dir);
    let mut peer = FakePeer::connect(&node);
    peer.send(&Message::GetAddr);
    assert!(peer.is_closed(Duration::from_secs(5)));
    stop(node, dir);
}

#[test]
fn oversized_frame_is_refused() {
    let dir = temp_dir("big");
    let node = start(&dir);
    let mut peer = FakePeer::connect(&node);
    let mut frame = magic().to_vec();
    frame.push(8);
    frame.extend_from_slice(&u32::MAX.to_le_bytes());
    frame.extend_from_slice(&[0; 4]);
    peer.stream.write_all(&frame).unwrap();
    assert!(peer.is_closed(Duration::from_secs(5)));
    stop(node, dir);
}

#[test]
fn mutated_block_gets_peer_disconnected_but_not_the_real_block() {
    let dir = temp_dir("mutated");
    let node = start(&dir);
    // Un bloc valide, miné à part.
    let src_dir = temp_dir("mutated-src");
    let source = start(&src_dir);
    let key = kynovara_core::crypto::KeyPair::from_secret_bytes(&[1; 32]);
    let addr = kynovara_core::address::Address::from_public_key(Network::Regtest, &key.public_key());
    let hash = source.generate(1, &addr).unwrap()[0];
    let block: Block = {
        // Récupère le bloc via un pair « honnête » : on le redemande au nœud source.
        let mut honest = FakePeer::connect(&source);
        honest.handshake();
        honest.send(&Message::GetData(vec![InvItem { kind: InvKind::Block, hash }]));
        loop {
            match honest.recv(Duration::from_secs(5)) {
                Some(Message::Block(b)) => break *b,
                Some(_) => {}
                None => panic!("bloc non reçu"),
            }
        }
    };

    // Le pair malveillant envoie une version altérée (même en-tête, transactions modifiées).
    let mut evil = FakePeer::connect(&node);
    evil.handshake();
    let mut mutated = block.clone();
    mutated.transactions[0].outputs[0].value -= 1;
    evil.send(&Message::Inv(vec![InvItem { kind: InvKind::Block, hash }]));
    // Le nœud demande le bloc ; on répond par la version altérée.
    loop {
        match evil.recv(Duration::from_secs(5)) {
            Some(Message::GetData(_)) => break,
            Some(_) => {}
            None => panic!("pas de demande de bloc"),
        }
    }
    evil.send(&Message::Block(Box::new(mutated)));
    assert!(evil.is_closed(Duration::from_secs(5)), "le pair malveillant doit être déconnecté");
    assert_eq!(node.tip().0, 0);

    // Le vrai bloc, avec la même empreinte d'en-tête, est toujours accepté.
    let mut honest = FakePeer::connect(&node);
    honest.handshake();
    honest.send(&Message::Inv(vec![InvItem { kind: InvKind::Block, hash }]));
    loop {
        match honest.recv(Duration::from_secs(5)) {
            Some(Message::GetData(_)) => break,
            Some(_) => {}
            None => panic!("pas de demande de bloc"),
        }
    }
    honest.send(&Message::Block(Box::new(block)));
    let deadline = Instant::now() + Duration::from_secs(5);
    while node.tip().1 != hash && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(node.tip(), (1, hash));
    stop(source, src_dir);
    stop(node, dir);
}

#[test]
fn getdata_for_unknown_items_gets_notfound() {
    let dir = temp_dir("notfound");
    let node = start(&dir);
    let mut peer = FakePeer::connect(&node);
    peer.handshake();
    let unknown = |i: u32| {
        let mut h = [0x5a; 32];
        h[..4].copy_from_slice(&i.to_le_bytes());
        Hash256(h)
    };
    let items: Vec<InvItem> = (0..2000u32).map(|i| InvItem { kind: InvKind::Block, hash: unknown(i) }).collect();
    peer.send(&Message::GetData(items));
    // Le même objet demandé 2 000 fois n'est traité (et répondu) qu'une fois.
    peer.send(&Message::GetData(vec![InvItem { kind: InvKind::Tx, hash: unknown(7) }; 2000]));
    let mut answers = Vec::new();
    while answers.len() < 2 {
        match peer.recv(Duration::from_secs(5)) {
            Some(Message::NotFound(list)) => answers.push(list.len()),
            Some(_) => {}
            None => panic!("pas de réponse NotFound"),
        }
    }
    assert_eq!(answers, vec![2000, 1]);
    // La connexion reste ouverte : demander des objets inconnus n'est pas une faute.
    assert!(!peer.is_closed(Duration::from_millis(500)));
    stop(node, dir);
}

#[test]
fn self_connection_is_detected() {
    let dir = temp_dir("self");
    let node = start(&dir);
    node.add_peer(node.p2p_addr().unwrap());
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(node.connected_peers(), 0);
    stop(node, dir);
}

/// Une branche concurrente plus légère n'est jamais écrite sur le disque (un
/// attaquant ne peut pas le remplir de branches bon marché) ; dès qu'elle
/// devient plus lourde, elle est écrite et adoptée.
#[test]
fn lighter_side_branch_never_reaches_the_disk() {
    use kynovara_core::chain::{Chain, MemoryStore};
    use kynovara_core::mempool::{Mempool, MempoolPolicy};
    use kynovara_core::miner::{create_block_template, mine_header};

    let dir = temp_dir("sidebranch");
    // Marge nulle : toute branche plus légère que la chaîne active compte comme
    // « lointaine » (par défaut, seules les fourches de plus de 1 000 blocs).
    let mut c = config(&dir);
    c.side_branch_margin = 0;
    let node = Node::start(c).unwrap();
    let key = kynovara_core::crypto::KeyPair::from_secret_bytes(&[1; 32]);
    let addr = kynovara_core::address::Address::from_public_key(Network::Regtest, &key.public_key());
    node.generate(5, &addr).unwrap();
    let blk = dir.join("regtest").join("blocks").join("blk00000.dat");
    let disk_before = std::fs::metadata(&blk).unwrap().len();

    // Branche concurrente depuis la genèse, minée à part.
    let mut other = Chain::new(Network::Regtest.params(), MemoryStore::default()).unwrap();
    let pool = Mempool::new(MempoolPolicy::default());
    let other_key = kynovara_core::crypto::KeyPair::from_secret_bytes(&[2; 32]);
    let mut side = Vec::new();
    for _ in 0..6 {
        let now = other.tip().header.timestamp + 1;
        let mut block = create_block_template(&other, &pool, other_key.public_key().pubkey_hash(), now, b"x").block;
        mine_header(&mut block.header, 1_000_000).unwrap();
        other.process_block(block.clone(), u64::MAX / 2).unwrap();
        side.push(block);
    }

    let mut peer = FakePeer::connect(&node);
    peer.handshake();
    let tip = node.tip();
    // 5 blocs : au mieux autant de travail que la chaîne du nœud → en mémoire seulement.
    for b in &side[..5] {
        peer.send(&Message::Block(Box::new(b.clone())));
    }
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(node.tip(), tip, "la chaîne active ne change pas");
    assert_eq!(std::fs::metadata(&blk).unwrap().len(), disk_before, "rien d'écrit sur le disque");
    assert!(!peer.is_closed(Duration::from_millis(200)), "des blocs valides ne sont pas une faute");

    // Le 6e bloc rend la branche plus lourde : écrite sur le disque et adoptée.
    peer.send(&Message::Block(Box::new(side[5].clone())));
    let deadline = Instant::now() + Duration::from_secs(5);
    while node.tip().1 != side[5].hash() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(node.tip(), (6, side[5].hash()));
    assert!(std::fs::metadata(&blk).unwrap().len() > disk_before);
    node.check_consistency().unwrap();
    stop(node, dir);
}

#[test]
fn genesis_block_resubmission_is_harmless() {
    let dir = temp_dir("genesis");
    let node = start(&dir);
    let g = genesis_block(Network::Regtest.params());
    assert_eq!(node.submit_block(g).unwrap(), kynovara_node::BlockResult::AlreadyKnown);
    stop(node, dir);
}
