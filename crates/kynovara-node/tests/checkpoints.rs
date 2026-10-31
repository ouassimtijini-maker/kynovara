//! Points de contrôle signés entre vrais nœuds (regtest) : un attaquant
//! majoritaire ne réécrit pas l'historique signé, une signature
//! contradictoire gèle le réseau jusqu'à la décision de la clé maîtresse, une
//! clé révoquée ne signe plus, et tout survit à un redémarrage.
#![allow(clippy::unwrap_used)]

use kynovara_core::address::Address;
use kynovara_core::checkpoint::{Checkpoint, Delegation, Revocation};
use kynovara_core::crypto::KeyPair;
use kynovara_core::hash::Hash256;
use kynovara_core::mempool::MempoolError;
use kynovara_core::params::{Network, REGTEST_CHECKPOINT_SECRET};
use kynovara_core::tx::{Transaction, TxOut};
use kynovara_node::checkpoints::CheckpointStatus;
use kynovara_node::logger::Level;
use kynovara_node::{Node, NodeConfig};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn temp_dir(name: &str) -> PathBuf {
    let mut b = [0u8; 8];
    getrandom::getrandom(&mut b).unwrap();
    let dir = std::env::temp_dir().join(format!("kynovara-cp-{name}-{:x}", u64::from_le_bytes(b)));
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

fn stop(node: Node) {
    node.shutdown();
    node.wait();
}

fn addr(i: u8) -> Address {
    Address::from_public_key(Network::Regtest, &KeyPair::from_secret_bytes(&[i; 32]).public_key())
}

fn master() -> KeyPair {
    KeyPair::from_secret_bytes(&REGTEST_CHECKPOINT_SECRET)
}

fn genesis() -> Hash256 {
    Network::Regtest.params().genesis.hash.unwrap()
}

const HOT: [u8; 32] = [42; 32];

fn delegation(max_height: u64) -> Delegation {
    Delegation::new(&master(), &genesis(), KeyPair::from_secret_bytes(&HOT).public_key(), max_height)
}

fn latest_height(node: &Node) -> Option<u64> {
    node.checkpoint_status()["latest"]["height"].as_u64()
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

#[test]
fn majority_attacker_cannot_rewrite_signed_history() {
    let (da, db, dm) = (temp_dir("a"), temp_dir("b"), temp_dir("m"));
    let founder = start(&da);
    let honest = start(&db);
    let attacker = start(&dm);

    // Le fondateur signe chaque nouveau bloc.
    founder.install_signer(&HOT, delegation(1_000)).unwrap();
    founder.generate(20, &addr(1)).unwrap();
    assert_eq!(latest_height(&founder), Some(20));

    honest.add_peer(founder.p2p_addr().unwrap());
    let tip = founder.tip();
    wait_until("synchronisation et point de contrôle du nœud honnête", 30, || {
        honest.tip() == tip && latest_height(&honest) == Some(20)
    });

    // L'attaquant a miné en secret une chaîne deux fois plus lourde.
    attacker.generate(40, &addr(9)).unwrap();
    assert!(attacker.tip().0 > tip.0);
    attacker.add_peer(honest.p2p_addr().unwrap());

    // Le nœud honnête refuse de basculer ; c'est l'attaquant qui, recevant le
    // point de contrôle, est ramené sur la chaîne signée.
    wait_until("l'attaquant suit la chaîne signée", 30, || attacker.tip() == tip);
    assert_eq!(honest.tip(), tip);
    honest.check_consistency().unwrap();
    attacker.check_consistency().unwrap();

    for (n, d) in [(founder, da), (honest, db), (attacker, dm)] {
        stop(n);
        let _ = std::fs::remove_dir_all(d);
    }
}

#[test]
fn stolen_key_contradiction_freezes_until_master_decides() {
    let dir = temp_dir("alert");
    let node = start(&dir);
    node.install_signer(&HOT, delegation(1_000)).unwrap();
    node.generate(110, &addr(1)).unwrap();
    let (height, tip) = node.tip();
    assert_eq!(latest_height(&node), Some(height));

    // Un voleur de la clé de signature signe un autre bloc à la hauteur 50.
    let hot = KeyPair::from_secret_bytes(&HOT);
    let forged = Checkpoint::sign_delegate(&hot, delegation(1_000), &genesis(), 50, Hash256([0xee; 32]));
    assert!(matches!(node.submit_checkpoint(forged).unwrap(), CheckpointStatus::Conflict(_)));
    assert!(node.checkpoint_alert().is_some());

    // Alerte : plus de signature, chaîne gelée, transactions suspendues.
    node.generate(1, &addr(1)).unwrap();
    assert_eq!(latest_height(&node), Some(height), "plus aucune signature pendant l'alerte");
    let tx = Transaction::transfer(vec![], vec![TxOut { value: 1, recipient: addr(2).pubkey_hash() }]);
    assert_eq!(node.submit_tx(tx).unwrap_err(), MempoolError::Suspended);
    let (h1, t1) = node.tip();
    let other = Checkpoint::sign_delegate(&hot, delegation(1_000), &genesis(), h1, t1);
    assert_eq!(node.submit_checkpoint(other).unwrap(), CheckpointStatus::IgnoredDuringAlert);
    let _ = tip;

    // La clé maîtresse révoque la clé volée et tranche.
    let rev = Revocation::new(&master(), &genesis(), hot.public_key());
    assert!(node.submit_revocation(rev).unwrap());
    assert!(node.checkpoint_status()["signer"].is_null(), "notre clé révoquée n'est plus utilisée");
    let (h, t) = node.tip();
    let decision = Checkpoint::sign_master(&master(), &genesis(), h, t);
    assert_eq!(node.submit_checkpoint(decision).unwrap(), CheckpointStatus::Applied);
    assert!(node.checkpoint_alert().is_none());
    // La clé révoquée ne peut plus rien signer de nouveau (un point déjà
    // couvert par l'historique signé est simplement ignoré).
    node.generate(1, &addr(1)).unwrap();
    let (h2, t2) = node.tip();
    assert_eq!(latest_height(&node), Some(h), "plus de clé de signature : rien de signé");
    let late = Checkpoint::sign_delegate(&hot, delegation(1_000), &genesis(), h2, t2);
    assert!(node.submit_checkpoint(late).is_err());
    let covered = Checkpoint::sign_delegate(&hot, delegation(1_000), &genesis(), h, t);
    assert_eq!(node.submit_checkpoint(covered).unwrap(), CheckpointStatus::AlreadyKnown);
    // Un faux maître non plus.
    let fake = Checkpoint::sign_master(&hot, &genesis(), h2, t2);
    assert!(node.submit_checkpoint(fake).is_err());

    // Une nouvelle clé de signature reprend le travail.
    let fresh = [43u8; 32];
    let d = Delegation::new(&master(), &genesis(), KeyPair::from_secret_bytes(&fresh).public_key(), 1_000);
    node.install_signer(&fresh, d).unwrap();
    node.generate(2, &addr(1)).unwrap();
    assert_eq!(latest_height(&node), Some(node.tip().0));
    node.check_consistency().unwrap();
    stop(node);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn checkpoints_alerts_and_signer_survive_a_restart() {
    let dir = temp_dir("restart");
    let node = start(&dir);
    node.install_signer(&HOT, delegation(1_000)).unwrap();
    node.generate(8, &addr(1)).unwrap();
    let tip = node.tip();
    stop(node);

    let node = start(&dir);
    assert_eq!(node.tip(), tip);
    assert_eq!(latest_height(&node), Some(tip.0));
    assert!(!node.checkpoint_status()["signer"].is_null(), "clé de signature rechargée");
    node.generate(1, &addr(1)).unwrap();
    assert_eq!(latest_height(&node), Some(tip.0 + 1));

    // Une alerte enregistrée reste en vigueur après redémarrage.
    let hot = KeyPair::from_secret_bytes(&HOT);
    let forged = Checkpoint::sign_delegate(&hot, delegation(1_000), &genesis(), 3, Hash256([0xee; 32]));
    assert!(matches!(node.submit_checkpoint(forged).unwrap(), CheckpointStatus::Conflict(_)));
    stop(node);
    let node = start(&dir);
    assert!(node.checkpoint_alert().is_some());
    assert_eq!(node.checkpoint_status()["latest"]["height"].as_u64(), Some(tip.0 + 1));
    stop(node);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn delegation_limits_are_enforced() {
    let dir = temp_dir("limits");
    let node = start(&dir);
    // Délégation non signée par la clé maîtresse : refusée.
    let thief = KeyPair::from_secret_bytes(&[7; 32]);
    let fake = Delegation::new(&thief, &genesis(), thief.public_key(), 100);
    assert!(node.install_signer(&[7; 32], fake).is_err());
    // Clé secrète ne correspondant pas à la délégation : refusée.
    assert!(node.install_signer(&[8; 32], delegation(100)).is_err());
    // Délégation courte : le nœud cesse de signer à son échéance.
    node.install_signer(&HOT, delegation(5)).unwrap();
    node.generate(7, &addr(1)).unwrap();
    assert_eq!(latest_height(&node), Some(5));
    stop(node);
    let _ = std::fs::remove_dir_all(dir);
}
