//! Tests d'intégration de la chaîne : validation, réorganisations, orphelins.
#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use kynovara_core::amount::COIN;
use kynovara_core::block::Block;
use kynovara_core::chain::{BlockStatus, Chain, MemoryStore, ProcessOutcome};
use kynovara_core::encode::Encode;
use kynovara_core::hash::Hash256;
use kynovara_core::miner::mine_header;
use kynovara_core::tx::{OutPoint, TxOut};
use kynovara_core::u256::U256;
use kynovara_core::validation::{BlockError, TxError};

fn assert_block_error(result: Result<ProcessOutcome, BlockError>, check: impl Fn(&BlockError) -> bool) {
    match result {
        Err(e) => assert!(check(&e), "erreur inattendue : {e:?}"),
        Ok(ProcessOutcome::Accepted { invalid, .. }) if !invalid.is_empty() => {
            assert!(check(&invalid[0].1), "erreur inattendue : {:?}", invalid[0].1)
        }
        Ok(other) => panic!("le bloc aurait dû être refusé : {other:?}"),
    }
}

#[test]
fn mine_mature_and_spend() {
    let mut chain = new_chain();
    let a = key(1);
    mine_empty(&mut chain, 100, pkh(1));
    assert_eq!(chain.height(), 100);
    chain.check_consistency().unwrap();

    let (op, out) = coinbase_output(&chain, 1);
    assert_eq!(out.value, 50 * COIN);
    let tx = spend(
        &chain.genesis_hash(),
        &a,
        &[(op, out)],
        vec![TxOut { value: 20 * COIN, recipient: pkh(2) }, TxOut { value: 29 * COIN, recipient: pkh(1) }],
    );

    // Le bloc 101 peut dépenser la récompense du bloc 1 (maturité 100).
    let block = make_block(&chain, chain.tip_hash(), vec![tx.clone()], pkh(3), COIN, 0);
    extend(&mut chain, block);
    assert_eq!(chain.tx_height(&tx.txid()), Some(101));
    assert!(chain.utxo().get(&op).is_none());
    assert_eq!(chain.utxo().for_recipient(&pkh(2))[0].1.output.value, 20 * COIN);
    // Le mineur a touché les frais (1 KYNO).
    let (_, cb) = coinbase_output(&chain, 101);
    assert_eq!(cb.value, 51 * COIN);
    chain.check_consistency().unwrap();
}

#[test]
fn immature_coinbase_is_rejected() {
    let mut chain = new_chain();
    mine_empty(&mut chain, 99, pkh(1));
    let (op, out) = coinbase_output(&chain, 1);
    let tx = spend(&chain.genesis_hash(), &key(1), &[(op, out)], vec![TxOut { value: COIN, recipient: pkh(2) }]);
    // Hauteur 100 : 1 + 100 > 100, encore immature.
    let block = make_block(&chain, chain.tip_hash(), vec![tx], pkh(1), 0, 0);
    assert_block_error(submit(&mut chain, block), |e| {
        matches!(e, BlockError::Transaction { error: TxError::ImmatureCoinbase { .. }, .. })
    });
    assert_eq!(chain.height(), 99);
    chain.check_consistency().unwrap();
}

#[test]
fn double_spends_are_rejected() {
    let mut chain = new_chain();
    mine_empty(&mut chain, 101, pkh(1));
    let g = chain.genesis_hash();
    let coin = coinbase_output(&chain, 1);
    let tx1 = spend(&g, &key(1), &[coin], vec![TxOut { value: 10 * COIN, recipient: pkh(2) }]);
    let tx2 = spend(&g, &key(1), &[coin], vec![TxOut { value: 11 * COIN, recipient: pkh(3) }]);

    // Dans le même bloc.
    let block = make_block(&chain, chain.tip_hash(), vec![tx1.clone(), tx2.clone()], pkh(1), 0, 0);
    assert_block_error(submit(&mut chain, block), |e| {
        matches!(e, BlockError::Transaction { error: TxError::MissingInput(_), .. })
    });
    chain.check_consistency().unwrap();

    // Dans deux blocs successifs.
    let block = make_block(&chain, chain.tip_hash(), vec![tx1], pkh(1), 0, 0);
    extend(&mut chain, block);
    let block = make_block(&chain, chain.tip_hash(), vec![tx2], pkh(1), 0, 0);
    assert_block_error(submit(&mut chain, block), |e| {
        matches!(e, BlockError::Transaction { error: TxError::MissingInput(_), .. })
    });
    chain.check_consistency().unwrap();
}

#[test]
fn spending_output_created_in_same_block_is_allowed() {
    let mut chain = new_chain();
    mine_empty(&mut chain, 101, pkh(1));
    let g = chain.genesis_hash();
    let coin = coinbase_output(&chain, 1);
    let tx1 = spend(&g, &key(1), &[coin], vec![TxOut { value: 50 * COIN, recipient: pkh(2) }]);
    let child_input = (OutPoint { txid: tx1.txid(), vout: 0 }, tx1.outputs[0]);
    let tx2 = spend(&g, &key(2), &[child_input], vec![TxOut { value: 50 * COIN, recipient: pkh(3) }]);
    // Le second dépense le premier : l'ordre compte.
    let wrong_order = make_block(&chain, chain.tip_hash(), vec![tx2.clone(), tx1.clone()], pkh(1), 0, 1);
    assert_block_error(submit(&mut chain, wrong_order), |e| {
        matches!(e, BlockError::Transaction { error: TxError::MissingInput(_), .. })
    });
    let block = make_block(&chain, chain.tip_hash(), vec![tx1, tx2], pkh(1), 0, 2);
    extend(&mut chain, block);
    assert_eq!(chain.utxo().for_recipient(&pkh(3)).len(), 1);
    assert!(chain.utxo().for_recipient(&pkh(2)).is_empty());
    chain.check_consistency().unwrap();
}

#[test]
fn signature_key_and_amount_rules() {
    let mut chain = new_chain();
    mine_empty(&mut chain, 101, pkh(1));
    let g = chain.genesis_hash();
    let coin = coinbase_output(&chain, 1);

    // Mauvaise clé (clé 2 pour une pièce de la clé 1).
    let tx = spend(&g, &key(2), &[coin], vec![TxOut { value: COIN, recipient: pkh(2) }]);
    let b = make_block(&chain, chain.tip_hash(), vec![tx], pkh(1), 0, 0);
    assert_block_error(submit(&mut chain, b), |e| {
        matches!(e, BlockError::Transaction { error: TxError::WrongPublicKey(0), .. })
    });

    // Signature d'une autre transaction.
    let mut tx = spend(&g, &key(1), &[coin], vec![TxOut { value: COIN, recipient: pkh(2) }]);
    tx.outputs[0].value = 2 * COIN;
    let b = make_block(&chain, chain.tip_hash(), vec![tx], pkh(1), 0, 0);
    assert_block_error(submit(&mut chain, b), |e| {
        matches!(e, BlockError::Transaction { error: TxError::InvalidSignature(0), .. })
    });

    // Sorties supérieures aux entrées.
    let tx = spend(&g, &key(1), &[coin], vec![TxOut { value: 51 * COIN, recipient: pkh(2) }]);
    let b = make_block(&chain, chain.tip_hash(), vec![tx], pkh(1), 0, 0);
    assert_block_error(submit(&mut chain, b), |e| {
        matches!(e, BlockError::Transaction { error: TxError::InsufficientInputs { .. }, .. })
    });

    // Signature d'un autre réseau (genèse différente).
    let other_genesis = Hash256([0x11; 32]);
    let tx = spend(&other_genesis, &key(1), &[coin], vec![TxOut { value: COIN, recipient: pkh(2) }]);
    let b = make_block(&chain, chain.tip_hash(), vec![tx], pkh(1), 0, 0);
    assert_block_error(submit(&mut chain, b), |e| {
        matches!(e, BlockError::Transaction { error: TxError::InvalidSignature(0), .. })
    });
    assert_eq!(chain.height(), 101);
    chain.check_consistency().unwrap();
}

#[test]
fn coinbase_rules() {
    let mut chain = new_chain();
    let tip = chain.tip_hash();
    // Récompense trop élevée d'une unité.
    let b = make_block(&chain, tip, vec![], pkh(1), 1, 0);
    assert_block_error(submit(&mut chain, b), |e| matches!(e, BlockError::CoinbaseValueTooHigh { .. }));
    // Mauvaise hauteur engagée.
    let mut b = make_block(&chain, tip, vec![], pkh(1), 0, 0);
    b.transactions[0] =
        kynovara_core::tx::Transaction::coinbase(7, vec![], vec![TxOut { value: COIN, recipient: pkh(1) }]);
    b.header.merkle_root = b.compute_merkle_root();
    mine_header(&mut b.header, 1_000_000).unwrap();
    assert_block_error(submit(&mut chain, b), |e| matches!(e, BlockError::CoinbaseHeightMismatch { .. }));
    // Un mineur peut réclamer moins que permis.
    let mut b = make_block(&chain, tip, vec![], pkh(1), 0, 0);
    b.transactions[0].outputs[0].value = 1;
    b.header.merkle_root = b.compute_merkle_root();
    mine_header(&mut b.header, 1_000_000).unwrap();
    extend(&mut chain, b);
}

#[test]
fn header_context_rules() {
    let mut chain = new_chain();
    let tip = chain.tip_hash();
    let tip_time = chain.tip().header.timestamp;

    // Cible incorrecte (plus facile ou plus difficile que prévu).
    let mut b = make_block(&chain, tip, vec![], pkh(1), 0, 0);
    b.header.target = U256::low_bits(254);
    mine_header(&mut b.header, 1_000_000).unwrap();
    assert_block_error(submit(&mut chain, b), |e| matches!(e, BlockError::BadTarget { .. }));

    // Horodatage égal à celui du parent.
    let mut b = make_block(&chain, tip, vec![], pkh(1), 0, 0);
    b.header.timestamp = tip_time;
    mine_header(&mut b.header, 1_000_000).unwrap();
    assert_block_error(submit(&mut chain, b), |e| matches!(e, BlockError::TimestampNotIncreasing { .. }));

    // Trop loin dans le futur : refus temporaire, le bloc n'est pas marqué.
    let b = make_block(&chain, tip, vec![], pkh(1), 0, 0);
    let now = b.header.timestamp - chain.params().max_future_block_time - 1;
    let err = chain.process_block(b.clone(), now).unwrap_err();
    assert!(matches!(err, BlockError::TimestampTooFarInFuture { .. }));
    assert!(!err.is_permanent());
    // Plus tard, le même bloc est accepté.
    extend(&mut chain, b);
}

#[test]
fn structure_rules_and_mutation() {
    let mut chain = new_chain();
    mine_empty(&mut chain, 101, pkh(1));
    let g = chain.genesis_hash();
    let coin = coinbase_output(&chain, 1);
    let tx = spend(&g, &key(1), &[coin], vec![TxOut { value: COIN, recipient: pkh(2) }]);
    let good = make_block(&chain, chain.tip_hash(), vec![tx.clone()], pkh(1), 49 * COIN, 0);

    // Transaction modifiée en route : la racine de Merkle ne correspond plus.
    let mut mutated = good.clone();
    mutated.transactions[1].outputs[0].value = 2 * COIN;
    let err = submit(&mut chain, mutated).unwrap_err();
    assert_eq!(err, BlockError::BadMerkleRoot);
    assert!(!err.is_permanent());
    // Le bloc original (même empreinte d'en-tête) reste acceptable.
    assert_eq!(good.hash(), good.header.hash());
    extend(&mut chain, good);

    // Transaction en double.
    let tx = spend(&g, &key(1), &[coinbase_output(&chain, 2)], vec![TxOut { value: COIN, recipient: pkh(2) }]);
    let b = make_block(&chain, chain.tip_hash(), vec![tx.clone(), tx], pkh(1), 0, 0);
    assert_block_error(submit(&mut chain, b), |e| matches!(e, BlockError::DuplicateTransaction(_)));

    // Preuve de travail insuffisante.
    let mut b = make_block(&chain, chain.tip_hash(), vec![], pkh(1), 0, 0);
    while b.header.meets_target() {
        b.header.nonce += 1;
    }
    assert_eq!(submit(&mut chain, b).unwrap_err(), BlockError::InsufficientWork);
    chain.check_consistency().unwrap();
}

#[test]
fn reorganisation_to_heavier_branch_and_back() {
    let mut chain = new_chain();
    mine_empty(&mut chain, 3, pkh(1));
    let fork_point = chain.block_hash_at(1).unwrap();
    let main_tip = chain.tip_hash();

    // Branche B depuis la hauteur 1 : 2 blocs (même travail que A, pas de bascule).
    let mut parent = fork_point;
    for i in 0..2 {
        let b = make_block(&chain, parent, vec![], pkh(2), 0, 10 + i);
        parent = b.hash();
        let outcome = submit(&mut chain, b).unwrap();
        assert!(matches!(outcome, ProcessOutcome::Accepted { tip_change: None, .. }));
    }
    assert_eq!(chain.tip_hash(), main_tip, "à travail égal, la première chaîne vue reste active");

    // Un bloc de plus sur B : B devient plus lourde, réorganisation.
    let b = make_block(&chain, parent, vec![], pkh(2), 0, 20);
    let tc = extend(&mut chain, b);
    assert_eq!(tc.disconnected.len(), 2);
    assert_eq!(tc.connected.len(), 3);
    assert_eq!(chain.height(), 4);
    assert_eq!(chain.utxo().for_recipient(&pkh(1)).len(), 1);
    assert_eq!(chain.utxo().for_recipient(&pkh(2)).len(), 3);
    chain.check_consistency().unwrap();

    // A reprend la tête avec 3 blocs de plus.
    let mut parent = main_tip;
    for i in 0..3 {
        let b = make_block(&chain, parent, vec![], pkh(1), 0, 30 + i);
        parent = b.hash();
        submit(&mut chain, b).unwrap();
    }
    assert_eq!(chain.tip_hash(), parent);
    assert_eq!(chain.height(), 6);
    assert_eq!(chain.utxo().for_recipient(&pkh(2)).len(), 0);
    chain.check_consistency().unwrap();
}

#[test]
fn reorg_moves_transactions_out_of_chain() {
    let mut chain = new_chain();
    mine_empty(&mut chain, 101, pkh(1));
    let g = chain.genesis_hash();
    let fork = chain.tip_hash();
    let tx = spend(&g, &key(1), &[coinbase_output(&chain, 1)], vec![TxOut { value: COIN, recipient: pkh(5) }]);
    let with_tx = make_block(&chain, fork, vec![tx.clone()], pkh(1), 49 * COIN, 0);
    extend(&mut chain, with_tx);
    assert_eq!(chain.tx_height(&tx.txid()), Some(102));

    let b1 = make_block(&chain, fork, vec![], pkh(2), 0, 1);
    let b1h = b1.hash();
    submit(&mut chain, b1).unwrap();
    let b2 = make_block(&chain, b1h, vec![], pkh(2), 0, 2);
    let tc = extend(&mut chain, b2);
    assert_eq!(tc.disconnected.len(), 1);
    assert_eq!(tc.disconnected[0].transactions[1], tx);
    assert_eq!(chain.tx_height(&tx.txid()), None);
    assert!(chain.utxo().get(&coinbase_output(&chain, 1).0).is_some());
    chain.check_consistency().unwrap();
}

#[test]
fn invalid_heavier_branch_is_rejected() {
    let mut chain = new_chain();
    mine_empty(&mut chain, 101, pkh(1));
    let g = chain.genesis_hash();
    let fork = chain.block_hash_at(100).unwrap();
    let main_tip = chain.tip_hash();

    // Branche : bloc invalide (dépense d'une pièce inexistante) puis 3 blocs.
    let ghost = (OutPoint { txid: Hash256([0x77; 32]), vout: 0 }, TxOut { value: COIN, recipient: pkh(1) });
    let bad_tx = spend(&g, &key(1), &[ghost], vec![TxOut { value: COIN, recipient: pkh(9) }]);
    let bad = make_block(&chain, fork, vec![bad_tx], pkh(9), 0, 1);
    let bad_hash = bad.hash();
    // Même travail que la chaîne active : stocké, pas encore essayé.
    let outcome = submit(&mut chain, bad.clone()).unwrap();
    assert!(matches!(outcome, ProcessOutcome::Accepted { tip_change: None, .. }));
    assert_eq!(chain.entry(&bad_hash).unwrap().status, BlockStatus::Pending);
    // Un enfant rend la branche plus lourde : elle est essayée puis rejetée.
    let child = make_block(&chain, bad_hash, vec![], pkh(9), 0, 2);
    let child_hash = child.hash();
    match submit(&mut chain, child).unwrap() {
        ProcessOutcome::Accepted { invalid, tip_change } => {
            assert!(invalid.iter().any(|(h, _)| *h == bad_hash), "{invalid:?}");
            assert!(tip_change.is_none());
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(chain.tip_hash(), main_tip);
    assert_eq!(chain.entry(&bad_hash).unwrap().status, BlockStatus::Invalid);
    assert_eq!(chain.entry(&child_hash).unwrap().status, BlockStatus::Invalid);
    // Le bloc invalide est reconnu comme tel ; ses descendants sont refusés.
    assert_eq!(submit(&mut chain, bad).unwrap_err(), BlockError::KnownInvalid);
    let grandchild = make_block(&chain, child_hash, vec![], pkh(9), 0, 9);
    assert!(matches!(submit(&mut chain, grandchild).unwrap_err(), BlockError::InvalidAncestor(_)));
    chain.check_consistency().unwrap();

    // La chaîne valide continue normalement.
    mine_empty(&mut chain, 5, pkh(1));
    assert_eq!(chain.height(), 106);
    chain.check_consistency().unwrap();
}

#[test]
fn orphans_are_connected_when_parent_arrives() {
    let mut source = new_chain();
    let blocks = mine_empty(&mut source, 6, pkh(1));
    let mut chain = new_chain();
    // Réception dans le désordre : 6, 5, 4, 3, 2 puis 1.
    for b in blocks.iter().skip(1).rev() {
        match submit(&mut chain, b.clone()).unwrap() {
            ProcessOutcome::Orphan { missing_parent } => assert_eq!(missing_parent, b.header.prev_hash),
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(chain.orphan_count(), 5);
    assert!(matches!(submit(&mut chain, blocks[3].clone()).unwrap(), ProcessOutcome::AlreadyKnown));
    match submit(&mut chain, blocks[0].clone()).unwrap() {
        ProcessOutcome::Accepted { tip_change: Some(tc), invalid } => {
            assert!(invalid.is_empty());
            assert_eq!(tc.connected.len(), 6);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(chain.height(), 6);
    assert_eq!(chain.tip_hash(), source.tip_hash());
    assert_eq!(chain.orphan_count(), 0);
    chain.check_consistency().unwrap();
}

#[test]
fn deep_reorg_beyond_undo_depth() {
    let mut chain = new_chain();
    chain.set_undo_depth(3);
    mine_empty(&mut chain, 101, pkh(1));
    let g = chain.genesis_hash();
    let fork = chain.block_hash_at(90).unwrap();
    // Une dépense sur la chaîne principale, qui doit disparaître après la réorganisation.
    let tx = spend(&g, &key(1), &[coinbase_output(&chain, 1)], vec![TxOut { value: COIN, recipient: pkh(4) }]);
    let b = make_block(&chain, chain.tip_hash(), vec![tx.clone()], pkh(1), 49 * COIN, 0);
    extend(&mut chain, b);

    let mut parent = fork;
    for i in 0..15 {
        let b = make_block(&chain, parent, vec![], pkh(2), 0, 50 + i);
        parent = b.hash();
        submit(&mut chain, b).unwrap();
    }
    assert_eq!(chain.tip_hash(), parent);
    assert_eq!(chain.height(), 105);
    assert_eq!(chain.tx_height(&tx.txid()), None);
    assert!(chain.utxo().for_recipient(&pkh(4)).is_empty());
    chain.check_consistency().unwrap();
}

#[test]
fn side_branch_stays_in_memory_until_it_wins() {
    use kynovara_core::chain::BlockStore;
    let mut chain = new_chain();
    // Marge nulle : toute branche plus légère que la chaîne active reste en mémoire.
    chain.set_side_branch_margin(0);
    mine_empty(&mut chain, 3, pkh(1));
    let fork = chain.block_hash_at(1).unwrap();
    let main_blocks: Vec<Hash256> = (1..=3).map(|h| chain.block_hash_at(h).unwrap()).collect();

    // Deux blocs sur une branche qui ne dépasse pas la chaîne active : indexés,
    // mais pas écrits sur le disque.
    let mut parent = fork;
    let mut side = Vec::new();
    for i in 0..2 {
        let b = make_block(&chain, parent, vec![], pkh(2), 0, 40 + i);
        parent = b.hash();
        side.push(parent);
        submit(&mut chain, b).unwrap();
    }
    for h in &side {
        assert!(chain.contains(h) && chain.is_parked(h), "bloc en mémoire attendu");
        assert!(!chain.store().contains(h), "un bloc qui ne gagne pas ne doit pas toucher le disque");
        assert!(chain.get_block(h).unwrap().is_some(), "toujours consultable");
    }
    assert_eq!(chain.parked_stats().0, 2);

    // Un troisième bloc rend la branche plus lourde : toute la branche est
    // écrite (ancêtres d'abord), puis la réorganisation a lieu.
    let b = make_block(&chain, parent, vec![], pkh(2), 0, 50);
    let winner = b.hash();
    extend(&mut chain, b);
    assert_eq!(chain.tip_hash(), winner);
    for h in side.iter().chain(std::iter::once(&winner)).chain(main_blocks.iter()) {
        assert!(chain.store().contains(h));
        assert!(!chain.is_parked(h));
    }
    assert_eq!(chain.parked_stats(), (0, 0));
    chain.check_consistency().unwrap();
}

#[test]
fn parked_pool_forgets_cheapest_branches_first() {
    let mut chain = new_chain();
    chain.set_side_branch_margin(0);
    mine_empty(&mut chain, 10, pkh(1));
    // Branche « bon marché » partant de loin (hauteurs 2 à 4 : peu de travail cumulé).
    let mut parent = chain.block_hash_at(1).unwrap();
    let mut deep = Vec::new();
    for i in 0..3 {
        let b = make_block(&chain, parent, vec![], pkh(3), 0, 60 + i);
        parent = b.hash();
        deep.push(parent);
        submit(&mut chain, b).unwrap();
    }
    // Branche concurrente récente (hauteur 9 : travail proche de la chaîne active).
    let s9 = make_block(&chain, chain.block_hash_at(8).unwrap(), vec![], pkh(4), 0, 70);
    let s9h = s9.hash();
    submit(&mut chain, s9).unwrap();
    assert_eq!(chain.parked_stats().0, 4);

    // Réserve limitée à 2 blocs : la branche profonde est oubliée par le haut.
    chain.set_parked_limits(2, usize::MAX);
    assert_eq!(chain.parked_stats().0, 2);
    assert!(chain.contains(&deep[0]) && !chain.contains(&deep[1]) && !chain.contains(&deep[2]));
    assert!(chain.contains(&s9h));
    // Renvoyer un bloc oublié ne déloge pas une branche plus intéressante.
    let again = make_block(&chain, deep[0], vec![], pkh(3), 0, 61);
    assert_eq!(again.hash(), deep[1]);
    submit(&mut chain, again).unwrap();
    assert!(!chain.contains(&deep[1]) && chain.contains(&s9h));

    // La branche récente gagne : elle est écrite et devient active.
    let s10 = make_block(&chain, s9h, vec![], pkh(4), 0, 71);
    let s10h = s10.hash();
    submit(&mut chain, s10).unwrap();
    assert!(chain.is_parked(&s10h), "travail égal : pas encore de bascule");
    assert!(!chain.contains(&deep[0]), "la feuille la moins lourde est oubliée");
    let s11 = make_block(&chain, s10h, vec![], pkh(4), 0, 72);
    let s11h = s11.hash();
    extend(&mut chain, s11);
    assert_eq!(chain.tip_hash(), s11h);
    assert_eq!(chain.parked_stats(), (0, 0));
    chain.check_consistency().unwrap();

    // Limite en octets : la réserve ne la dépasse jamais ; une fois pleine, le
    // haut de la branche la moins lourde (ici le nouveau bloc) est refusé.
    chain.set_parked_limits(usize::MAX, 600);
    let mut parent = chain.block_hash_at(2).unwrap();
    let mut kept = 0;
    for i in 0..10 {
        let b = make_block(&chain, parent, vec![], pkh(5), 0, 80 + i);
        let h = b.hash();
        submit(&mut chain, b).unwrap();
        assert!(chain.parked_stats().1 <= 600, "{:?}", chain.parked_stats());
        if !chain.contains(&h) {
            break;
        }
        kept += 1;
        parent = h;
    }
    assert!((1..10).contains(&kept), "{kept}");
    chain.check_consistency().unwrap();
}

#[test]
fn recent_forks_go_to_disk_deep_lighter_forks_stay_in_memory() {
    use kynovara_core::chain::BlockStore;
    let mut chain = new_chain();
    chain.set_side_branch_margin(3);
    mine_empty(&mut chain, 10, pkh(1));
    // Fourche récente (depuis la hauteur 8, dans la marge) : sur le disque tout
    // de suite, même plus légère que la chaîne active.
    let near = make_block(&chain, chain.block_hash_at(8).unwrap(), vec![], pkh(2), 0, 1);
    submit(&mut chain, near.clone()).unwrap();
    assert!(chain.store().contains(&near.hash()) && !chain.is_parked(&near.hash()));
    // Fourche profonde (depuis la hauteur 2) : en mémoire seulement…
    let mut parent = chain.block_hash_at(2).unwrap();
    let mut deep = Vec::new();
    for i in 0..5 {
        let b = make_block(&chain, parent, vec![], pkh(3), 0, 10 + i);
        parent = b.hash();
        deep.push(parent);
        submit(&mut chain, b).unwrap();
    }
    // … jusqu'à revenir dans la course (hauteur 8 = 10 - 3 + 1) : la branche
    // entière est alors écrite, ancêtres d'abord.
    assert!(deep[..4].iter().all(|h| chain.is_parked(h)), "hauteurs 3 à 6 en mémoire");
    assert!(chain.store().contains(&deep[4]) || chain.is_parked(&deep[4]));
    let b = make_block(&chain, parent, vec![], pkh(3), 0, 20);
    let h8 = b.hash();
    submit(&mut chain, b).unwrap();
    assert!(chain.store().contains(&h8));
    assert!(deep.iter().all(|h| chain.store().contains(h) && !chain.is_parked(h)));
    assert_eq!(chain.parked_stats(), (0, 0));
    chain.check_consistency().unwrap();
}

#[test]
fn blocks_already_on_disk_are_never_parked() {
    use kynovara_core::chain::BlockStore;
    // Blocs d'une chaîne source : 3 blocs principaux et une branche de 2 blocs.
    let mut source = new_chain();
    let main = mine_empty(&mut source, 3, pkh(1));
    let b2 = make_block(&source, main[0].hash(), vec![], pkh(2), 0, 1);
    submit(&mut source, b2.clone()).unwrap();
    let b3 = make_block(&source, b2.hash(), vec![], pkh(2), 0, 2);
    submit(&mut source, b3.clone()).unwrap();

    // Relecture au démarrage : tous les blocs sont déjà sur le disque.
    let mut store = MemoryStore::default();
    for b in main.iter().chain([&b2, &b3]) {
        store.put(&b.hash(), b).unwrap();
    }
    let mut chain = Chain::new(regtest(), store).unwrap();
    for b in main.iter().chain([&b2, &b3]) {
        submit(&mut chain, b.clone()).unwrap();
    }
    assert_eq!(chain.tip_hash(), main[2].hash());
    assert_eq!(chain.parked_stats(), (0, 0));
    assert!(chain.contains(&b3.hash()) && !chain.is_parked(&b3.hash()));
}

#[test]
fn huge_reorg_report_is_bounded() {
    let mut chain = new_chain();
    chain.set_tip_change_limits(4, usize::MAX);
    mine_empty(&mut chain, 10, pkh(1));
    let fork = chain.block_hash_at(2).unwrap();
    let old: Vec<Hash256> = (3..=10).map(|h| chain.block_hash_at(h).unwrap()).collect();
    // Branche de 9 blocs depuis la hauteur 2 : 8 retirés, 9 ajoutés.
    let mut parent = fork;
    let mut tc = None;
    for i in 0..9 {
        let b = make_block(&chain, parent, vec![], pkh(2), 0, 90 + i);
        parent = b.hash();
        if let ProcessOutcome::Accepted { tip_change: Some(change), .. } = submit(&mut chain, b).unwrap() {
            tc = Some(change);
        }
    }
    let tc = tc.unwrap();
    assert_eq!(chain.height(), 11);
    assert!(tc.truncated);
    // Seuls les blocs les plus hauts sont rapportés, dans le bon ordre.
    let gone: Vec<Hash256> = tc.disconnected.iter().map(Block::hash).collect();
    assert_eq!(gone, old.iter().rev().take(4).copied().collect::<Vec<_>>());
    let added: Vec<u64> = tc.connected.iter().map(|b| chain.entry(&b.hash()).unwrap().height).collect();
    assert_eq!(added, vec![8, 9, 10, 11]);
    chain.check_consistency().unwrap();

    // Un petit changement reste complet.
    let b = make_block(&chain, chain.tip_hash(), vec![], pkh(2), 0, 99);
    let small = extend(&mut chain, b);
    assert!(!small.truncated && small.connected.len() == 1);
}

#[test]
fn locator_and_sync_helpers() {
    let mut chain = new_chain();
    mine_empty(&mut chain, 50, pkh(1));
    let loc = chain.locator();
    assert_eq!(loc[0], chain.tip_hash());
    assert_eq!(*loc.last().unwrap(), chain.genesis_hash());
    assert!(loc.len() < 30);
    // Un pair qui n'a que la genèse reçoit les 10 premiers blocs.
    let after = chain.hashes_after_locator(&[chain.genesis_hash()], &Hash256::ZERO, 10);
    assert_eq!(after.len(), 10);
    assert_eq!(after[0], chain.block_hash_at(1).unwrap());
    // Arrêt sur `stop`.
    let stop = chain.block_hash_at(5).unwrap();
    let after = chain.hashes_after_locator(&[chain.genesis_hash()], &stop, 100);
    assert_eq!(after.len(), 5);
    // Localisateur inconnu : on repart de la genèse.
    let after = chain.hashes_after_locator(&[Hash256([1; 32])], &Hash256::ZERO, 1000);
    assert_eq!(after.len(), 50);
    assert_eq!(chain.ancestor(&chain.tip_hash(), 7), chain.block_hash_at(7));
}

#[test]
fn block_encoding_survives_storage_roundtrip() {
    let mut chain = new_chain();
    let blocks = mine_empty(&mut chain, 3, pkh(1));
    for b in blocks {
        let bytes = b.to_bytes();
        let decoded = <Block as kynovara_core::encode::Decode>::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, b);
    }
}

/// Test aléatoire : de nombreuses branches et dépenses, avec vérification
/// complète de l'état après chaque bloc.
#[test]
fn randomized_branches_keep_state_consistent() {
    let mut rng = 0x5eed_u64;
    let mut next = move || {
        rng = rng.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };
    let mut chain: Chain<MemoryStore> = new_chain();
    mine_empty(&mut chain, 110, pkh(1));
    let g = chain.genesis_hash();
    let mut tips = vec![chain.tip_hash()];
    for round in 0..120u32 {
        // Choisit une branche (parfois un ancêtre récent pour créer une fourche).
        let mut parent = tips[(next() as usize) % tips.len()];
        if next() % 4 == 0 {
            let h = chain.entry(&parent).unwrap().height;
            if let Some(a) = chain.ancestor(&parent, h.saturating_sub(next() % 3)) {
                parent = a;
            }
        }
        // Parfois une dépense d'une récompense mature de la chaîne active.
        let mut txs = Vec::new();
        let mut fees = 0;
        if next() % 2 == 0 && chain.is_active(&parent) {
            let ph = chain.entry(&parent).unwrap().height;
            let h = 1 + next() % (ph.saturating_sub(100).max(1));
            let coin = coinbase_output(&chain, h);
            if chain.utxo().get(&coin.0).is_some() && coin.1.recipient == pkh(1) {
                txs.push(spend(&g, &key(1), &[coin], vec![TxOut { value: coin.1.value - 1000, recipient: pkh(2) }]));
                fees = 1000;
            }
        }
        let b = make_block(&chain, parent, txs, pkh(1), fees, (round % 251) as u8);
        let h = b.hash();
        let _ = submit(&mut chain, b);
        if chain.entry(&h).is_some() {
            tips.retain(|t| *t != parent);
            tips.push(h);
        }
        chain.check_consistency().unwrap();
        // La chaîne active a le travail maximal parmi les blocs valides.
        let best = tips
            .iter()
            .filter_map(|t| chain.entry(t))
            .filter(|e| e.status != BlockStatus::Invalid)
            .map(|e| e.chain_work)
            .max()
            .unwrap();
        assert!(chain.tip().chain_work >= best);
    }
    assert!(chain.height() > 115);
}
