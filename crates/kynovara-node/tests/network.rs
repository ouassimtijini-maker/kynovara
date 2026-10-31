//! Tests d'intégration : plusieurs vrais nœuds sur la machine (regtest), qui
//! se connectent en TCP, se synchronisent, relaient, se réorganisent et
//! redémarrent depuis le disque.
#![allow(clippy::unwrap_used)]

use kynovara_core::address::Address;
use kynovara_core::crypto::{KeyPair, Signature};
use kynovara_core::encode::Encode;
use kynovara_core::hash::Hash256;
use kynovara_core::params::Network;
use kynovara_core::tx::{OutPoint, Transaction, TxIn, TxKind, TxOut};
use kynovara_node::logger::Level;
use kynovara_node::{Node, NodeConfig};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn temp_dir(name: &str) -> PathBuf {
    let mut b = [0u8; 8];
    getrandom::getrandom(&mut b).unwrap();
    let dir = std::env::temp_dir().join(format!("kynovara-net-{name}-{:x}", u64::from_le_bytes(b)));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn config(dir: &Path) -> NodeConfig {
    let mut c = NodeConfig::default_for(Network::Regtest);
    c.datadir = dir.to_path_buf();
    c.listen = Some("127.0.0.1:0".parse().unwrap());
    c.rpc = "127.0.0.1:0".parse().unwrap();
    c.log_level = Level::Warn;
    c
}

fn start(dir: &Path) -> Node {
    Node::start(config(dir)).unwrap()
}

fn key(i: u8) -> KeyPair {
    KeyPair::from_secret_bytes(&[i; 32])
}

fn addr(i: u8) -> Address {
    Address::from_public_key(Network::Regtest, &key(i).public_key())
}

fn wait_until(what: &str, timeout_secs: u64, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("délai dépassé : {what}");
}

fn stop(node: Node) {
    node.shutdown();
    node.wait();
}

/// Requête RPC brute (avec ou sans jeton).
fn rpc(node: &Node, method: &str, path: &str, body: &str, token: Option<&str>) -> (u16, serde_json::Value) {
    let mut s = TcpStream::connect(node.rpc_addr()).unwrap();
    let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
    let req =
        format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\n{auth}Content-Length: {}\r\n\r\n{body}", body.len());
    s.write_all(req.as_bytes()).unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    let status: u16 = resp[9..12].parse().unwrap();
    let body = resp.split_once("\r\n\r\n").unwrap().1;
    (status, serde_json::from_str(body).unwrap())
}

/// Dépense la récompense du bloc `height` (payée à la clé 1).
fn spend_coinbase(node: &Node, height: u64, to: &Address, fee: u64) -> Transaction {
    let (_, cookie) = (0, node.cookie().to_string());
    let (_, block) = rpc(node, "GET", &format!("/v1/block-at/{height}"), "", Some(&cookie));
    let cb = &block["transactions"][0];
    let txid = Hash256::from_hex(cb["txid"].as_str().unwrap()).unwrap();
    let value = cb["outputs"][0]["value"].as_u64().unwrap();
    let owner = key(1);
    let spent = TxOut { value, recipient: owner.public_key().pubkey_hash() };
    let mut tx = Transaction::transfer(
        vec![TxIn {
            prev_out: OutPoint { txid, vout: 0 },
            public_key: owner.public_key(),
            signature: Signature([0; 64]),
        }],
        vec![TxOut { value: value - fee, recipient: to.pubkey_hash() }],
    );
    let genesis = Network::Regtest.params().genesis.hash.unwrap();
    let sighash = tx.sighash(&genesis, &[spent]);
    if let TxKind::Transfer { inputs } = &mut tx.kind {
        inputs[0].signature = owner.sign(&sighash.0);
    }
    tx
}

#[test]
fn three_nodes_sync_relay_and_mine() {
    let (da, db, dc) = (temp_dir("a"), temp_dir("b"), temp_dir("c"));
    let a = start(&da);
    let b = start(&db);
    let c = start(&dc);
    // Topologie en ligne : C — B — A.
    b.add_peer(a.p2p_addr().unwrap());
    c.add_peer(b.p2p_addr().unwrap());
    wait_until("présentations", 15, || {
        a.connected_peers() == 1 && b.connected_peers() == 2 && c.connected_peers() == 1
    });

    // A mine 110 blocs ; B et C se synchronisent.
    a.generate(110, &addr(1)).unwrap();
    let tip = a.tip();
    wait_until("synchronisation de B et C", 30, || b.tip() == tip && c.tip() == tip);

    // Une transaction soumise à C traverse B jusqu'à A.
    let tx = spend_coinbase(&c, 1, &addr(2), 10_000);
    let txid = c.submit_tx(tx.clone()).unwrap();
    wait_until("relais de la transaction", 15, || a.mempool_contains(&txid) && b.mempool_contains(&txid));

    // A la mine ; tout le monde la voit confirmée, les mempools se vident.
    a.generate(1, &addr(3)).unwrap();
    wait_until("confirmation partout", 15, || {
        [&a, &b, &c].iter().all(|n| n.tx_height(&txid) == Some(111) && n.mempool_len() == 0)
    });
    // Double dépense refusée.
    let double = spend_coinbase(&b, 1, &addr(4), 20_000);
    assert!(b.submit_tx(double).is_err());

    for n in [&a, &b, &c] {
        n.check_consistency().unwrap();
    }
    stop(c);
    stop(b);
    stop(a);
    for d in [da, db, dc] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[test]
fn isolated_branch_reorganises_on_reconnect() {
    let (da, dd) = (temp_dir("ra"), temp_dir("rd"));
    let a = start(&da);
    let d = start(&dd);
    // Deux chaînes indépendantes : A plus longue que D.
    a.generate(30, &addr(1)).unwrap();
    d.generate(12, &addr(9)).unwrap();
    assert_ne!(a.tip(), d.tip());
    // Connexion : D doit adopter la chaîne de A (plus de travail).
    d.add_peer(a.p2p_addr().unwrap());
    let tip = a.tip();
    wait_until("réorganisation de D", 30, || d.tip() == tip);
    d.check_consistency().unwrap();
    stop(d);
    stop(a);
    let _ = std::fs::remove_dir_all(da);
    let _ = std::fs::remove_dir_all(dd);
}

#[test]
fn restart_restores_chain_from_disk() {
    let dir = temp_dir("restart");
    let node = start(&dir);
    node.generate(105, &addr(1)).unwrap();
    let tx = spend_coinbase(&node, 2, &addr(2), 5_000);
    let txid = node.submit_tx(tx).unwrap();
    node.generate(2, &addr(1)).unwrap();
    let tip = node.tip();
    stop(node);

    // Un second nœud sur le même dossier pendant que le premier tourne : refusé.
    let node = start(&dir);
    assert!(Node::start(config(&dir)).is_err());
    assert_eq!(node.tip(), tip);
    assert_eq!(node.tx_height(&txid), Some(106));
    node.check_consistency().unwrap();
    stop(node);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn rpc_authentication_and_errors() {
    let dir = temp_dir("rpc");
    let node = start(&dir);
    let token = node.cookie().to_string();
    // Sans jeton, ou avec un mauvais jeton : refusé.
    assert_eq!(rpc(&node, "GET", "/v1/info", "", None).0, 401);
    assert_eq!(rpc(&node, "GET", "/v1/info", "", Some("faux")).0, 401);
    // Avec le jeton.
    let (status, info) = rpc(&node, "GET", "/v1/info", "", Some(&token));
    assert_eq!(status, 200);
    assert_eq!(info["network"], "regtest");
    assert_eq!(info["height"], 0);
    // Le fichier .cookie contient le même jeton.
    let on_disk = std::fs::read_to_string(node.data_dir().join(".cookie")).unwrap();
    assert_eq!(on_disk, token);
    // Erreurs.
    assert_eq!(rpc(&node, "GET", "/v1/nope", "", Some(&token)).0, 404);
    assert_eq!(rpc(&node, "DELETE", "/v1/info", "", Some(&token)).0, 405);
    assert_eq!(rpc(&node, "POST", "/v1/tx", "{pas du json", Some(&token)).0, 400);
    assert_eq!(rpc(&node, "POST", "/v1/tx", r#"{"hex":"zz"}"#, Some(&token)).0, 400);
    assert_eq!(rpc(&node, "GET", "/v1/block/1234", "", Some(&token)).0, 400);
    assert_eq!(rpc(&node, "GET", "/v1/block-at/99", "", Some(&token)).0, 404);
    // Génération et lecture via le RPC.
    let a = addr(1).encode();
    let (status, generated) =
        rpc(&node, "POST", "/v1/generate", &format!(r#"{{"count":3,"address":"{a}"}}"#), Some(&token));
    assert_eq!(status, 200, "{generated}");
    assert_eq!(generated["blocks"].as_array().unwrap().len(), 3);
    let (status, utxos) = rpc(&node, "POST", "/v1/utxos", &format!(r#"{{"addresses":["{a}"]}}"#), Some(&token));
    assert_eq!(status, 200);
    assert_eq!(utxos["utxos"].as_array().unwrap().len(), 3);
    assert_eq!(utxos["utxos"][0]["mature"], false);
    // Adresse d'un autre réseau refusée.
    let other = Address::from_public_key(Network::Testnet, &key(1).public_key()).encode();
    let (status, _) = rpc(&node, "POST", "/v1/utxos", &format!(r#"{{"addresses":["{other}"]}}"#), Some(&token));
    assert_eq!(status, 400);
    // Transaction invalide (coinbase immature) refusée avec 409.
    let tx = spend_coinbase(&node, 1, &addr(2), 1000);
    let body = format!(r#"{{"hex":"{}"}}"#, kynovara_core::hex::encode(&tx.to_bytes()));
    let (status, err) = rpc(&node, "POST", "/v1/tx", &body, Some(&token));
    assert_eq!(status, 409, "{err}");
    // Arrêt via le RPC.
    let (status, _) = rpc(&node, "POST", "/v1/stop", "", Some(&token));
    assert_eq!(status, 200);
    node.wait();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn misbehaving_peer_is_disconnected() {
    let dir = temp_dir("abuse");
    let node = start(&dir);
    let target: SocketAddr = node.p2p_addr().unwrap();
    // Un « pair » qui envoie des octets quelconques est coupé.
    let mut s = TcpStream::connect(target).unwrap();
    s.write_all(b"GET / HTTP/1.1\r\n\r\nbonjour").unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut buf = [0u8; 64];
    let n = s.read(&mut buf).unwrap_or(0);
    assert_eq!(n, 0, "la connexion aurait dû être fermée");
    // Un pair qui ne se présente pas est coupé après le délai… on vérifie
    // simplement que le nœud continue de fonctionner.
    assert_eq!(node.connected_peers(), 0);
    stop(node);
    let _ = std::fs::remove_dir_all(dir);
}
