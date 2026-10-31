//! Nœuds d'amorçage : un nœud neuf rejoint tout seul le réseau ; `--connect`
//! les écarte ; un nœud qui se retrouve dans sa propre liste ne s'y connecte
//! pas en boucle.
#![allow(clippy::unwrap_used)]

use kynovara_core::address::Address;
use kynovara_core::crypto::KeyPair;
use kynovara_core::params::Network;
use kynovara_node::logger::Level;
use kynovara_node::{Node, NodeConfig};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn temp_dir(name: &str) -> PathBuf {
    let mut b = [0u8; 8];
    getrandom::getrandom(&mut b).unwrap();
    let dir = std::env::temp_dir().join(format!("kynovara-seed-{name}-{:x}", u64::from_le_bytes(b)));
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

fn wait_until(what: &str, secs: u64, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("délai dépassé : {what}");
}

fn stop(node: Node, dir: PathBuf) {
    node.shutdown();
    node.wait();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_fresh_node_joins_the_network_through_its_seed() {
    let (ds, dn, dc) = (temp_dir("amorce"), temp_dir("neuf"), temp_dir("connect"));
    let seed = Node::start(config(&ds)).unwrap();
    let miner = Address::from_public_key(Network::Regtest, &KeyPair::from_secret_bytes(&[1; 32]).public_key());
    seed.generate(12, &miner).unwrap();
    let seed_addr = seed.p2p_addr().unwrap();

    // Aucune option : le nœud neuf rejoint le nœud d'amorçage et se synchronise.
    let mut c = config(&dn);
    c.seeds = vec![seed_addr];
    let fresh = Node::start(c).unwrap();
    let tip = seed.tip();
    wait_until("synchronisation par le nœud d'amorçage", 30, || fresh.tip() == tip);
    assert!(fresh.connected_peers() >= 1);

    // Avec --connect, seuls les pairs imposés sont joints.
    let mut c = config(&dc);
    c.seeds = vec![seed_addr];
    c.connect = vec!["127.0.0.1:1".parse().unwrap()];
    let isolated = Node::start(c).unwrap();
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(isolated.tip().0, 0, "aucune synchronisation hors des pairs imposés");
    assert_eq!(isolated.connected_peers(), 0);

    stop(isolated, dc);
    stop(fresh, dn);
    stop(seed, ds);
}

#[test]
fn a_seed_that_is_ourselves_is_dropped() {
    let dir = temp_dir("soi");
    let node = Node::start(config(&dir)).unwrap();
    let own = node.p2p_addr().unwrap();
    // Comme le serveur d'amorçage, qui trouve sa propre adresse dans la liste.
    node.add_peer(own);
    wait_until("connexion à soi-même détectée", 20, || node.ignored_peers() == vec![own]);
    // Plus aucune tentative ensuite (sinon une reconnexion toutes les 5 s).
    std::thread::sleep(Duration::from_secs(7));
    assert_eq!(node.connected_peers(), 0);
    assert_eq!(node.ignored_peers(), vec![own]);
    stop(node, dir);
}
