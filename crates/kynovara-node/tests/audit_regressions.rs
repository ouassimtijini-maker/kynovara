//! Non-régression des constats de la relecture indépendante (voir AUDIT.md).
//!
//! Chaque test reprend la preuve de concept qui démontrait le problème, et
//! vérifie que le comportement corrigé tient.
#![allow(clippy::unwrap_used)]

use kynovara_core::address::Address;
use kynovara_core::crypto::{KeyPair, Signature};
use kynovara_core::hash::Hash256;
use kynovara_core::mempool::MempoolError;
use kynovara_core::params::Network;
use kynovara_core::tx::{OutPoint, Transaction, TxIn, TxKind, TxOut};
use kynovara_node::logger::Level;
use kynovara_node::p2p::message::{FrameReader, InvItem, InvKind, Message, VersionMessage};
use kynovara_node::{Node, NodeConfig};
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn temp_dir(name: &str) -> PathBuf {
    let mut b = [0u8; 8];
    getrandom::getrandom(&mut b).unwrap();
    let dir = std::env::temp_dir().join(format!("kynovara-audit-{name}-{:x}", u64::from_le_bytes(b)));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn start(dir: &Path) -> Node {
    let mut c = NodeConfig::default_for(Network::Regtest);
    c.datadir = dir.to_path_buf();
    c.listen = Some("127.0.0.1:0".parse().unwrap());
    c.rpc = "127.0.0.1:0".parse().unwrap();
    c.log_level = Level::Error;
    Node::start(c).unwrap()
}

fn stop(node: Node, dir: PathBuf) {
    node.shutdown();
    node.wait();
    let _ = std::fs::remove_dir_all(dir);
}

fn magic() -> [u8; 4] {
    Network::Regtest.params().magic
}

fn genesis() -> Hash256 {
    Network::Regtest.params().genesis.hash.unwrap()
}

fn key(i: u8) -> KeyPair {
    KeyPair::from_secret_bytes(&[i; 32])
}

fn addr(i: u8) -> Address {
    Address::from_public_key(Network::Regtest, &key(i).public_key())
}

fn rpc_get(node: &Node, path: &str) -> serde_json::Value {
    let mut s = TcpStream::connect(node.rpc_addr()).unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {}\r\n\r\n", node.cookie());
    s.write_all(req.as_bytes()).unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    serde_json::from_str(resp.split_once("\r\n\r\n").unwrap().1).unwrap()
}

/// Sortie de récompense (payée à la clé 1) du bloc actif `height`.
fn coinbase_of(node: &Node, height: u64) -> (OutPoint, TxOut) {
    let b = rpc_get(node, &format!("/v1/block-at/{height}"));
    let cb = &b["transactions"][0];
    let txid = Hash256::from_hex(cb["txid"].as_str().unwrap()).unwrap();
    let value = cb["outputs"][0]["value"].as_u64().unwrap();
    (OutPoint { txid, vout: 0 }, TxOut { value, recipient: key(1).public_key().pubkey_hash() })
}

fn sign(owner: &KeyPair, inputs: &[(OutPoint, TxOut)], outputs: Vec<TxOut>) -> Transaction {
    let mut tx = Transaction::transfer(
        inputs
            .iter()
            .map(|(op, _)| TxIn { prev_out: *op, public_key: owner.public_key(), signature: Signature([0; 64]) })
            .collect(),
        outputs,
    );
    let spent: Vec<TxOut> = inputs.iter().map(|(_, o)| *o).collect();
    let sighash = tx.sighash(&genesis(), &spent);
    if let TxKind::Transfer { inputs } = &mut tx.kind {
        for i in inputs.iter_mut() {
            i.signature = owner.sign(&sighash.0);
        }
    }
    tx
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

    fn handshake(&mut self) {
        self.send(&Message::Version(VersionMessage {
            version: 1,
            genesis: genesis(),
            best_height: 0,
            nonce: 4242,
            listen_port: 0,
            timestamp: 0,
            user_agent: "/audit:1/".into(),
        }));
        let (mut v, mut va) = (false, false);
        while !(v && va) {
            match self.recv(Duration::from_secs(5)) {
                Some(Message::Version(_)) => v = true,
                Some(Message::Verack) => va = true,
                Some(_) => {}
                None => panic!("présentation interrompue"),
            }
        }
        self.send(&Message::Verack);
    }

    /// Le nœud nous demande-t-il la transaction `txid` (GetData) avant `timeout` ?
    fn asked_for(&mut self, txid: Hash256, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(Message::GetData(items)) = self.recv(Duration::from_millis(200))
                && items.iter().any(|i| i.kind == InvKind::Tx && i.hash == txid)
            {
                return true;
            }
        }
        false
    }

    /// Attend la réponse à un ping (tous les messages précédents ont été traités).
    fn sync_ping(&mut self, n: u64, timeout: Duration) -> bool {
        self.send(&Message::Ping(n));
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(Message::Pong(x)) = self.recv(Duration::from_millis(500))
                && x == n
            {
                return true;
            }
        }
        false
    }
}

/// H-3 : une copie de T aux signatures altérées (même txid, autre wtxid) ne
/// doit pas empêcher le nœud de demander, puis d'accepter, la vraie T.
#[test]
fn malleated_copy_cannot_censor_the_real_transaction() {
    let dir = temp_dir("censor");
    let node = start(&dir);
    node.generate(102, &addr(1)).unwrap();
    let c1 = coinbase_of(&node, 1);
    let to = addr(2).pubkey_hash();
    let real = sign(&key(1), &[c1], vec![TxOut { value: c1.1.value - 10_000, recipient: to }]);
    let mut malleated = real.clone();
    if let TxKind::Transfer { inputs } = &mut malleated.kind {
        inputs[0].signature.0[5] ^= 0x01;
    }
    assert_eq!(malleated.txid(), real.txid());

    let mut honest = FakePeer::connect(&node);
    honest.handshake();
    let mut evil = FakePeer::connect(&node);
    evil.handshake();
    evil.send(&Message::Tx(Box::new(malleated)));
    assert!(evil.is_closed(Duration::from_secs(5)), "l'expéditeur d'une signature fausse est coupé");

    // Le pair honnête annonce la vraie transaction : elle est demandée…
    honest.send(&Message::Inv(vec![InvItem { kind: InvKind::Tx, hash: real.txid() }]));
    assert!(honest.asked_for(real.txid(), Duration::from_secs(3)), "la vraie transaction doit être demandée");
    // … et acceptée une fois reçue.
    honest.send(&Message::Tx(Box::new(real.clone())));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !node.mempool_contains(&real.txid()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(node.mempool_contains(&real.txid()));
    stop(node, dir);
}

/// H-1 : une transaction de centaines d'entrées sans frais est refusée sans
/// vérifier ses signatures (signatures volontairement fausses : refus pour
/// « frais », pas pour « signature ») ; la renvoyer en boucle fait couper le
/// pair par la limite de débit, qui compte les entrées.
#[test]
fn zero_fee_flood_is_cheap_to_refuse_and_cut_off() {
    let dir = temp_dir("sigdos");
    let node = start(&dir);
    node.generate(101, &addr(1)).unwrap();
    let c1 = coinbase_of(&node, 1);
    let me = key(1).public_key().pubkey_hash();
    let n: u64 = 740;
    let per = (c1.1.value - 100_000) / n;
    let fan = sign(&key(1), &[c1], (0..n).map(|_| TxOut { value: per, recipient: me }).collect());
    let fan_id = node.submit_tx(fan).unwrap();
    node.generate(1, &addr(3)).unwrap();

    let inputs: Vec<(OutPoint, TxOut)> =
        (0..n).map(|v| (OutPoint { txid: fan_id, vout: v as u32 }, TxOut { value: per, recipient: me })).collect();
    let mut zero_fee = sign(&key(1), &inputs, vec![TxOut { value: per * n, recipient: me }]);
    if let TxKind::Transfer { inputs } = &mut zero_fee.kind {
        for i in inputs.iter_mut() {
            i.signature = Signature([9; 64]);
        }
    }
    let err = node.submit_tx(zero_fee.clone()).unwrap_err();
    assert!(matches!(err, MempoolError::FeeTooLow { .. }), "refus pour les frais, avant les signatures : {err:?}");

    // Le même envoi en boucle par un pair : coupé (débit en fonction des entrées).
    let mut p = FakePeer::connect(&node);
    p.handshake();
    let frame = Message::Tx(Box::new(zero_fee)).to_frame(magic());
    for _ in 0..60 {
        if p.stream.write_all(&frame).is_err() {
            break;
        }
    }
    assert!(p.is_closed(Duration::from_secs(10)), "un pair qui inonde de grosses transactions est coupé");
    // Le nœud reste réactif.
    let t = Instant::now();
    let _ = node.tip();
    assert!(t.elapsed() < Duration::from_secs(1));
    stop(node, dir);
}

/// H-2 : demander des milliers de fois la même grosse transaction sans lire
/// les réponses ne fait plus gonfler la mémoire du nœud.
#[cfg(target_os = "linux")]
#[test]
fn getdata_flood_does_not_grow_memory() {
    fn rss_bytes() -> u64 {
        let s = std::fs::read_to_string("/proc/self/status").unwrap();
        let line = s.lines().find(|l| l.starts_with("VmRSS:")).unwrap();
        line.split_whitespace().nth(1).unwrap().parse::<u64>().unwrap() * 1024
    }
    let dir = temp_dir("queue");
    let node = start(&dir);
    node.generate(101, &addr(1)).unwrap();
    let c1 = coinbase_of(&node, 1);
    let me = key(1).public_key().pubkey_hash();
    let n: u64 = 2000;
    let per = (c1.1.value - 1_000_000) / n;
    let big = sign(&key(1), &[c1], (0..n).map(|_| TxOut { value: per, recipient: me }).collect());
    let id = node.submit_tx(big).unwrap();

    let mut evil = FakePeer::connect(&node);
    evil.handshake();
    std::thread::sleep(Duration::from_millis(300));
    let before = rss_bytes();
    let items = vec![InvItem { kind: InvKind::Tx, hash: id }; 2000];
    evil.send(&Message::GetData(items.clone()));
    evil.send(&Message::GetData(items));
    std::thread::sleep(Duration::from_secs(3));
    let grown = rss_bytes().saturating_sub(before);
    assert!(grown < 64_000_000, "mémoire du processus : +{} Mo", grown / 1_000_000);
    drop(evil);
    stop(node, dir);
}

/// M-4 : la limite d'adresses par source tient même en se reconnectant, et
/// une adresse interne fournie par un pair n'est jamais contactée.
#[test]
fn address_book_cannot_be_flooded_or_aimed_at_internal_services() {
    let dir = temp_dir("addr");
    let node = start(&dir);
    for round in 0..3u8 {
        let mut p = FakePeer::connect(&node);
        p.handshake();
        let addrs: Vec<SocketAddr> =
            (0..1000u16).map(|i| SocketAddr::from(([8, round, (i >> 8) as u8, i as u8], 19733))).collect();
        p.send(&Message::Addr(addrs));
        assert!(p.sync_ping(round.into(), Duration::from_secs(5)));
    }
    let datadir = node.data_dir().to_path_buf();
    node.shutdown();
    node.wait();
    let saved = std::fs::read_to_string(datadir.join("peers.txt")).unwrap_or_default();
    assert!(saved.lines().count() <= 1000, "{} adresses venant d'une seule source", saved.lines().count());
    let _ = std::fs::remove_dir_all(dir);

    // Un « service interne » sur la boucle locale, annoncé par un pair.
    let internal = TcpListener::bind("127.0.0.1:0").unwrap();
    internal.set_nonblocking(true).unwrap();
    let target = internal.local_addr().unwrap();
    let dir = temp_dir("ssrf");
    let node = start(&dir);
    let mut p = FakePeer::connect(&node);
    p.handshake();
    p.send(&Message::Addr(vec![target]));
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut contacted = false;
    while Instant::now() < deadline {
        if internal.accept().is_ok() {
            contacted = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(!contacted, "le nœud ne doit pas contacter une adresse interne fournie par un pair");
    stop(node, dir);
}

/// M-1 : une transaction soumise avant l'arrivée d'un pair lui parvient (la
/// mempool est annoncée à chaque nouveau pair).
#[test]
fn pending_transaction_reaches_peers_that_connect_later() {
    let (da, db) = (temp_dir("rb-a"), temp_dir("rb-b"));
    let a = start(&da);
    a.generate(101, &addr(1)).unwrap();
    let c1 = coinbase_of(&a, 1);
    let tx = sign(&key(1), &[c1], vec![TxOut { value: c1.1.value - 10_000, recipient: addr(2).pubkey_hash() }]);
    let txid = a.submit_tx(tx).unwrap(); // aucun pair à ce moment
    let b = start(&db);
    b.add_peer(a.p2p_addr().unwrap());
    let deadline = Instant::now() + Duration::from_secs(30);
    while !(b.tip() == a.tip() && b.mempool_contains(&txid)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(b.tip(), a.tip(), "B synchronisé");
    assert!(b.mempool_contains(&txid), "la transaction en attente doit parvenir au nouveau pair");
    // Un bloc miné par B la contient donc.
    b.generate(1, &addr(5)).unwrap();
    assert_eq!(b.tx_height(&txid), Some(102));
    stop(b, db);
    stop(a, da);
}
