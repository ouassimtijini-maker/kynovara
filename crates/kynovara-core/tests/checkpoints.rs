//! Points de contrôle signés appliqués à la chaîne : un attaquant majoritaire
//! ne peut plus réécrire ce qui précède le dernier point.
#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use kynovara_core::amount::COIN;
use kynovara_core::block::Block;
use kynovara_core::chain::{BlockStatus, CheckpointOutcome, ProcessOutcome};
use kynovara_core::hash::Hash256;
use kynovara_core::tx::{OutPoint, TxOut};
use kynovara_core::validation::BlockError;

/// Branche de `n` blocs à partir de `parent` (étiquette `tag` pour la distinguer).
fn branch(
    chain: &mut kynovara_core::chain::Chain<kynovara_core::chain::MemoryStore>,
    parent: Hash256,
    n: usize,
    tag: u8,
) -> Vec<Block> {
    let mut out = Vec::new();
    let mut parent = parent;
    for _ in 0..n {
        let b = make_block(chain, parent, vec![], pkh(9), 0, tag);
        parent = b.hash();
        let _ = submit(chain, b.clone());
        out.push(b);
    }
    out
}

#[test]
fn heavier_private_branch_cannot_rewrite_checkpointed_history() {
    let mut chain = new_chain();
    mine_empty(&mut chain, 10, pkh(1));
    let fork = chain.block_hash_at(5).unwrap();
    let cp = chain.block_hash_at(8).unwrap();
    assert!(matches!(chain.apply_checkpoint(8, cp, false), CheckpointOutcome::Applied { tip_change: None, .. }));
    assert_eq!(chain.checkpoint(), Some((8, cp)));

    // L'attaquant publie une branche bien plus lourde qui part d'avant le point.
    let attack = make_block(&chain, fork, vec![], pkh(9), 0, 7);
    assert_eq!(submit(&mut chain, attack.clone()).unwrap_err(), BlockError::ConflictsWithCheckpoint { height: 8 });
    assert!(chain.entry(&attack.hash()).is_none(), "ni stocké ni indexé");
    assert_eq!(chain.height(), 10);

    // Prolonger la chaîne au-delà du point reste possible, y compris par une
    // branche concurrente qui part après lui.
    let after = chain.block_hash_at(9).unwrap();
    let side = branch(&mut chain, after, 3, 3);
    assert_eq!(chain.tip_hash(), side[2].hash());
    chain.check_consistency().unwrap();
}

#[test]
fn checkpoint_on_a_lighter_branch_wins_over_work() {
    let mut chain = new_chain();
    mine_empty(&mut chain, 6, pkh(1));
    let fork = chain.block_hash_at(4).unwrap();
    let main_tip = chain.tip_hash();
    // Branche concurrente plus courte (stockée, pas active).
    let side = branch(&mut chain, fork, 1, 2);
    assert_eq!(chain.tip_hash(), main_tip);
    // Le fondateur a signé la branche courte : la chaîne y bascule.
    match chain.apply_checkpoint(5, side[0].hash(), false) {
        CheckpointOutcome::Applied { tip_change: Some(tc), invalid } => {
            assert!(invalid.is_empty());
            assert_eq!(tc.disconnected.len(), 2);
            assert_eq!(tc.connected.len(), 1);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(chain.tip_hash(), side[0].hash());
    assert!(!chain.respects_checkpoint(&main_tip));
    chain.check_consistency().unwrap();
    // L'ancienne branche, même prolongée, ne revient jamais.
    let late = make_block(&chain, main_tip, vec![], pkh(1), 0, 9);
    assert!(matches!(submit(&mut chain, late).unwrap_err(), BlockError::ConflictsWithCheckpoint { .. }));
}

#[test]
fn contradictions_are_detected_and_only_the_master_can_override() {
    let mut chain = new_chain();
    mine_empty(&mut chain, 6, pkh(1));
    let fork = chain.block_hash_at(3).unwrap();
    let side = branch(&mut chain, fork, 2, 4);
    let a = chain.block_hash_at(5).unwrap();
    assert!(matches!(chain.apply_checkpoint(5, a, false), CheckpointOutcome::Applied { .. }));

    // Même hauteur, autre bloc : contradiction (clé volée ?).
    assert!(matches!(chain.apply_checkpoint(5, side[1].hash(), false), CheckpointOutcome::Conflict { .. }));
    // Plus haut mais sur une autre branche : contradiction aussi.
    assert!(matches!(chain.apply_checkpoint(4, side[0].hash(), false), CheckpointOutcome::Conflict { .. }));
    // Plus ancien et cohérent : simplement déjà couvert.
    let older = chain.block_hash_at(2).unwrap();
    assert!(matches!(chain.apply_checkpoint(2, older, false), CheckpointOutcome::AlreadyKnown));
    // Hauteur fausse.
    assert!(matches!(chain.apply_checkpoint(6, a, false), CheckpointOutcome::Conflict { .. }));
    // Bloc inconnu au-delà du point : à redemander. En deçà : contradiction.
    assert!(matches!(chain.apply_checkpoint(9, Hash256([7; 32]), false), CheckpointOutcome::UnknownBlock));
    assert!(matches!(chain.apply_checkpoint(3, Hash256([7; 32]), false), CheckpointOutcome::Conflict { .. }));
    assert_eq!(chain.checkpoint(), Some((5, a)));

    // La clé maîtresse tranche : la chaîne bascule sur la branche désignée.
    assert!(matches!(chain.apply_checkpoint(5, side[1].hash(), true), CheckpointOutcome::Applied { .. }));
    assert_eq!(chain.tip_hash(), side[1].hash());
    chain.check_consistency().unwrap();
}

#[test]
fn checkpoint_on_an_invalid_block_is_refused_and_rolled_back() {
    let mut chain = new_chain();
    mine_empty(&mut chain, 101, pkh(1));
    let g = chain.genesis_hash();
    let good = chain.block_hash_at(100).unwrap();
    assert!(matches!(chain.apply_checkpoint(100, good, false), CheckpointOutcome::Applied { .. }));
    let main_tip = chain.tip_hash();
    // Bloc invalide (pièce inexistante) sur une branche partant du point.
    let ghost = (OutPoint { txid: Hash256([0x77; 32]), vout: 0 }, TxOut { value: COIN, recipient: pkh(1) });
    let bad_tx = spend(&g, &key(1), &[ghost], vec![TxOut { value: COIN, recipient: pkh(9) }]);
    let bad = make_block(&chain, good, vec![bad_tx], pkh(9), 0, 1);
    let bad_hash = bad.hash();
    assert!(matches!(submit(&mut chain, bad).unwrap(), ProcessOutcome::Accepted { .. }));
    assert_eq!(chain.entry(&bad_hash).unwrap().status, BlockStatus::Pending);

    match chain.apply_checkpoint(101, bad_hash, false) {
        CheckpointOutcome::Conflict { .. } => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(chain.checkpoint(), Some((100, good)), "point précédent rétabli");
    assert_eq!(chain.tip_hash(), main_tip);
    assert_eq!(chain.entry(&bad_hash).unwrap().status, BlockStatus::Invalid);
    chain.check_consistency().unwrap();
}

#[test]
fn frozen_chain_only_extends_its_tip() {
    let mut chain = new_chain();
    mine_empty(&mut chain, 5, pkh(1));
    let fork = chain.block_hash_at(3).unwrap();
    let main_tip = chain.tip_hash();
    chain.set_frozen(true);
    // Une branche plus lourde ne fait pas basculer une chaîne gelée…
    let side = branch(&mut chain, fork, 4, 5);
    assert_eq!(chain.tip_hash(), main_tip);
    // … mais le sommet peut toujours être prolongé.
    let next = make_block(&chain, main_tip, vec![], pkh(1), 0, 6);
    extend(&mut chain, next);
    assert!(chain.is_frozen());
    // Dégelée, elle rejoint la branche la plus lourde.
    chain.set_frozen(false);
    let more = make_block(&chain, side[3].hash(), vec![], pkh(9), 0, 7);
    let more_hash = more.hash();
    let _ = submit(&mut chain, more);
    assert_eq!(chain.tip_hash(), more_hash);
    chain.check_consistency().unwrap();
}

#[test]
fn release_checkpoints_protect_a_fresh_node_from_the_start() {
    use kynovara_core::chain::{Chain, MemoryStore};
    use kynovara_core::params::ConsensusParams;
    // Chaîne de référence (celle du fondateur) : on relève le bloc 3.
    let mut reference = new_chain();
    let honest = mine_empty(&mut reference, 6, pkh(1));
    let h3 = honest[2].hash();
    // Version du logiciel qui inscrit ce bloc.
    let params: &'static ConsensusParams = Box::leak(Box::new(ConsensusParams {
        release_checkpoints: Box::leak(vec![(3, h3)].into_boxed_slice()),
        ..regtest().clone()
    }));
    let mut fresh = Chain::new(params, MemoryStore::default()).unwrap();

    // Un attaquant lui sert d'abord une chaîne plus lourde, sans le bloc 3.
    let g = fresh.genesis_hash();
    let mut parent = g;
    let mut refused = 0;
    for i in 0..8 {
        let b = make_block(&fresh, parent, vec![], pkh(9), 0, 50 + i);
        parent = b.hash();
        match submit(&mut fresh, b) {
            Ok(_) => {}
            Err(BlockError::ConflictsWithCheckpoint { height: 3 }) => {
                refused += 1;
                break;
            }
            Err(e) => panic!("{e}"),
        }
    }
    assert_eq!(refused, 1, "le bloc 3 de l'attaquant est refusé");
    assert_eq!(fresh.height(), 2, "l'attaquant ne dépasse pas la hauteur 2");
    assert_eq!(fresh.checkpoint(), None);

    // Puis la vraie chaîne arrive : dès le bloc 3, elle s'impose.
    for b in &honest {
        let _ = submit(&mut fresh, b.clone());
    }
    assert_eq!(fresh.checkpoint(), Some((3, h3)));
    assert_eq!(fresh.tip_hash(), honest[5].hash());
    fresh.check_consistency().unwrap();

    // La chaîne de l'attaquant, même prolongée, ne revient plus.
    let late = make_block(&fresh, fresh.block_hash_at(1).unwrap(), vec![], pkh(9), 0, 99);
    assert!(matches!(submit(&mut fresh, late).unwrap_err(), BlockError::ConflictsWithCheckpoint { .. }));
}
