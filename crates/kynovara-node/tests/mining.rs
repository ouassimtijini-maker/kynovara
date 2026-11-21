//! Mineur intégré du nœud (regtest, SHA-256 double) : démarrage, blocs
//! trouvés, changement du nombre de fils, arrêt et redémarrage depuis le
//! disque. Le calcul RandomX des fils de minage est testé dans `mining.rs`.
#![allow(clippy::unwrap_used)]

use kynovara_core::address::Address;
use kynovara_core::crypto::KeyPair;
use kynovara_core::params::Network;
use kynovara_node::logger::Level;
use kynovara_node::{Node, NodeConfig};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn temp_dir(name: &str) -> PathBuf {
    let mut b = [0u8; 8];
    getrandom::getrandom(&mut b).unwrap();
    let dir = std::env::temp_dir().join(format!("kynovara-minage-{name}-{:x}", u64::from_le_bytes(b)));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn start(dir: &Path) -> Node {
    let mut c = NodeConfig::default_for(Network::Regtest);
    c.datadir = dir.to_path_buf();
    c.listen = None;
    c.rpc = "127.0.0.1:0".parse().unwrap();
    c.log_level = Level::Error;
    Node::start(c).unwrap()
}

fn addr(i: u8) -> Address {
    Address::from_public_key(Network::Regtest, &KeyPair::from_secret_bytes(&[i; 32]).public_key())
}

fn rpc_get(node: &Node, path: &str) -> serde_json::Value {
    let mut s = TcpStream::connect(node.rpc_addr()).unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {}\r\n\r\n", node.cookie());
    s.write_all(req.as_bytes()).unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    serde_json::from_str(resp.split_once("\r\n\r\n").unwrap().1).unwrap()
}

fn wait_for_height(node: &Node, height: u64) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while node.tip().0 < height {
        assert!(Instant::now() < deadline, "hauteur {height} non atteinte (actuelle : {})", node.tip().0);
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn builtin_miner_finds_blocks_stops_and_restarts() {
    let dir = temp_dir("fils");
    let node = start(&dir);
    let info = rpc_get(&node, "/v1/info");
    assert_eq!(info["pow"]["algorithm"], "sha256d");
    assert!(info["pow"]["randomx_activation_height"].is_null());

    node.start_mining(addr(1), 2).unwrap();
    assert_eq!(rpc_get(&node, "/v1/mining")["algorithm"], "sha256d");
    wait_for_height(&node, 5);

    // Arrêt : plus aucun bloc.
    node.stop_mining();
    std::thread::sleep(Duration::from_millis(500));
    let stopped_at = node.tip().0;
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(node.tip().0, stopped_at);

    // Reprise avec un autre nombre de fils.
    node.start_mining(addr(2), 3).unwrap();
    wait_for_height(&node, stopped_at + 3);
    node.stop_mining();
    std::thread::sleep(Duration::from_millis(300));
    node.check_consistency().unwrap();
    let (height, tip) = node.tip();
    node.shutdown();
    node.wait();

    // Redémarrage : la chaîne minée est relue depuis le disque.
    let node = start(&dir);
    assert_eq!(node.tip(), (height, tip));
    node.check_consistency().unwrap();
    node.shutdown();
    node.wait();
    let _ = std::fs::remove_dir_all(dir);
}
