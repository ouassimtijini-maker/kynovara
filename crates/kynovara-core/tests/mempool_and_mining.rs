//! Tests de la mempool, des modèles de blocs et de l'ajustement de difficulté
//! dans une vraie chaîne.
#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use kynovara_core::amount::COIN;
use kynovara_core::chain::{Chain, MemoryStore, ProcessOutcome};
use kynovara_core::genesis::genesis_header;
use kynovara_core::hash::Hash256;
use kynovara_core::mempool::{Mempool, MempoolError, MempoolPolicy};
use kynovara_core::miner::{create_block_template, mine_header};
use kynovara_core::params::{ConsensusParams, GenesisParams, Network};
use kynovara_core::tx::{OutPoint, TxOut};
use kynovara_core::u256::U256;
use kynovara_core::validation::TxError;

fn funded_chain() -> Chain<MemoryStore> {
    let mut chain = new_chain();
    mine_empty(&mut chain, 110, pkh(1));
    chain
}

fn accept(
    pool: &mut Mempool,
    chain: &Chain<MemoryStore>,
    tx: kynovara_core::tx::Transaction,
) -> Result<Hash256, MempoolError> {
    pool.accept(tx, chain.utxo(), chain.height() + 1, chain.params(), &chain.genesis_hash())
}

#[test]
fn mempool_accepts_and_rejects() {
    let chain = funded_chain();
    let g = chain.genesis_hash();
    let mut pool = Mempool::new(MempoolPolicy::default());
    let coin = coinbase_output(&chain, 1);

    let tx = spend(&g, &key(1), &[coin], vec![TxOut { value: 49 * COIN, recipient: pkh(2) }]);
    let txid = accept(&mut pool, &chain, tx.clone()).unwrap();
    assert_eq!(txid, tx.txid());
    assert_eq!(accept(&mut pool, &chain, tx), Err(MempoolError::AlreadyKnown));

    // Même pièce dépensée ailleurs.
    let conflict = spend(&g, &key(1), &[coin], vec![TxOut { value: 48 * COIN, recipient: pkh(3) }]);
    assert!(matches!(accept(&mut pool, &chain, conflict), Err(MempoolError::Conflict(_))));

    // Frais insuffisants (0 frais).
    let coin2 = coinbase_output(&chain, 2);
    let no_fee = spend(&g, &key(1), &[coin2], vec![TxOut { value: 50 * COIN, recipient: pkh(3) }]);
    assert!(matches!(accept(&mut pool, &chain, no_fee), Err(MempoolError::FeeTooLow { .. })));

    // Pièce inconnue.
    let ghost = (OutPoint { txid: Hash256([5; 32]), vout: 0 }, TxOut { value: COIN, recipient: pkh(1) });
    let tx = spend(&g, &key(1), &[ghost], vec![TxOut { value: COIN / 2, recipient: pkh(3) }]);
    let err = accept(&mut pool, &chain, tx).unwrap_err();
    assert!(matches!(err, MempoolError::Invalid(TxError::MissingInput(_))));
    assert!(!err.is_invalid_everywhere());

    // Récompense immature (bloc 105 : dépensable au bloc 205).
    let young = coinbase_output(&chain, 105);
    let tx = spend(&g, &key(1), &[young], vec![TxOut { value: COIN, recipient: pkh(3) }]);
    assert!(matches!(accept(&mut pool, &chain, tx), Err(MempoolError::Invalid(TxError::ImmatureCoinbase { .. }))));

    // Mauvaise signature : invalide partout (pair à pénaliser).
    let mut bad = spend(&g, &key(1), &[coinbase_output(&chain, 3)], vec![TxOut { value: COIN, recipient: pkh(3) }]);
    bad.outputs[0].value += 1;
    let err = accept(&mut pool, &chain, bad).unwrap_err();
    assert!(err.is_invalid_everywhere());
    assert_eq!(pool.len(), 1);
}

/// Non-régression (audit H-1) : les signatures, coûteuses, ne sont vérifiées
/// qu'après tout le reste. Une transaction sans frais aux signatures fausses
/// est refusée pour ses frais, sans qu'aucune signature ne soit vérifiée ; la
/// même avec des frais suffisants est refusée pour sa signature.
#[test]
fn signatures_are_checked_last() {
    let chain = funded_chain();
    let g = chain.genesis_hash();
    let mut pool = Mempool::new(MempoolPolicy::default());
    let coin = coinbase_output(&chain, 1);
    let mut no_fee = spend(&g, &key(1), &[coin], vec![TxOut { value: 50 * COIN, recipient: pkh(2) }]);
    if let kynovara_core::tx::TxKind::Transfer { inputs } = &mut no_fee.kind {
        inputs[0].signature = kynovara_core::crypto::Signature([7; 64]);
    }
    assert!(matches!(accept(&mut pool, &chain, no_fee), Err(MempoolError::FeeTooLow { .. })));
    let mut paying = spend(&g, &key(1), &[coin], vec![TxOut { value: 49 * COIN, recipient: pkh(2) }]);
    if let kynovara_core::tx::TxKind::Transfer { inputs } = &mut paying.kind {
        inputs[0].signature = kynovara_core::crypto::Signature([7; 64]);
    }
    let err = accept(&mut pool, &chain, paying).unwrap_err();
    assert_eq!(err, MempoolError::Invalid(TxError::InvalidSignature(0)));
    assert!(err.is_invalid_everywhere());
    assert!(pool.is_empty());

    // Le chemin en trois temps du nœud (vérification préalable, signatures
    // hors verrou, insertion) donne le même résultat que `accept`.
    let tx = spend(&g, &key(1), &[coin], vec![TxOut { value: 49 * COIN, recipient: pkh(2) }]);
    let checked = pool.precheck(&tx, chain.utxo(), chain.height() + 1, chain.params()).unwrap();
    kynovara_core::validation::verify_input_signatures(&tx, &checked.spent, &g).unwrap();
    let again = pool.precheck(&tx, chain.utxo(), chain.height() + 1, chain.params()).unwrap();
    assert_eq!(again, checked);
    assert_eq!(pool.insert_checked(tx.clone(), again), tx.txid());
    assert!(pool.contains(&tx.txid()));
    assert_eq!(pool.best_txids(10), vec![tx.txid()]);
}

#[test]
fn template_includes_mempool_and_pays_fees() {
    let mut chain = funded_chain();
    let g = chain.genesis_hash();
    let mut pool = Mempool::new(MempoolPolicy::default());
    let mut expected_fees = 0;
    for h in 1..=5u64 {
        let fee = h * 10_000;
        expected_fees += fee;
        let tx = spend(
            &g,
            &key(1),
            &[coinbase_output(&chain, h)],
            vec![TxOut { value: 50 * COIN - fee, recipient: pkh(2) }],
        );
        accept(&mut pool, &chain, tx).unwrap();
    }
    let template = create_block_template(&chain, &pool, pkh(7), chain.tip().header.timestamp + 1, b"test");
    assert_eq!(template.fees, expected_fees);
    assert_eq!(template.block.transactions.len(), 6);
    // Tri par frais par octet décroissants.
    let fees_in_order: Vec<u64> =
        template.block.transactions[1..].iter().map(|t| 50 * COIN - t.outputs[0].value).collect();
    assert!(fees_in_order.windows(2).all(|w| w[0] >= w[1]), "{fees_in_order:?}");
    let mut block = template.block;
    mine_header(&mut block.header, 1_000_000).unwrap();
    let outcome = submit(&mut chain, block.clone()).unwrap();
    assert!(matches!(outcome, ProcessOutcome::Accepted { tip_change: Some(_), .. }));
    let (_, cb) = coinbase_output(&chain, chain.height());
    assert_eq!(cb.value, 50 * COIN + expected_fees);

    // La mempool se vide des transactions confirmées.
    pool.remove_for_block(&block);
    assert!(pool.is_empty());
    assert_eq!(pool.total_bytes(), 0);
    chain.check_consistency().unwrap();
}

#[test]
fn mempool_revalidates_after_reorg() {
    let mut chain = funded_chain();
    let g = chain.genesis_hash();
    let mut pool = Mempool::new(MempoolPolicy::default());
    let fork = chain.tip_hash();
    // Une dépense confirmée sur la branche A…
    let coin = coinbase_output(&chain, 1);
    let tx_a = spend(&g, &key(1), &[coin], vec![TxOut { value: 49 * COIN, recipient: pkh(2) }]);
    let block_a = make_block(&chain, fork, vec![tx_a], pkh(1), COIN, 1);
    extend(&mut chain, block_a);
    // … puis une transaction en attente qui dépend de l'état de A.
    let child_coin = (
        OutPoint { txid: chain.get_block(&chain.tip_hash()).unwrap().unwrap().transactions[1].txid(), vout: 0 },
        TxOut { value: 49 * COIN, recipient: pkh(2) },
    );
    let tx_child = spend(&g, &key(2), &[child_coin], vec![TxOut { value: 48 * COIN, recipient: pkh(3) }]);
    accept(&mut pool, &chain, tx_child).unwrap();
    assert_eq!(pool.len(), 1);

    // La branche B (plus lourde) ne contient pas tx_a : tx_child devient invalide.
    let b1 = make_block(&chain, fork, vec![], pkh(4), 0, 2);
    let b1h = b1.hash();
    submit(&mut chain, b1).unwrap();
    let b2 = make_block(&chain, b1h, vec![], pkh(4), 0, 3);
    extend(&mut chain, b2);
    pool.revalidate(chain.utxo(), chain.height() + 1, chain.params());
    assert!(pool.is_empty());
}

#[test]
fn mempool_eviction_by_fee_rate() {
    let chain = funded_chain();
    let g = chain.genesis_hash();
    let mk = |h: u64, fee: u64| {
        spend(&g, &key(1), &[coinbase_output(&chain, h)], vec![TxOut { value: 50 * COIN - fee, recipient: pkh(2) }])
    };
    // Une transaction à 1 entrée et 1 sortie fait 173 octets.
    let size = mk(9, 0).size();
    assert_eq!(size, 173);
    let mut pool = Mempool::new(MempoolPolicy { min_fee_rate: 1, max_bytes: 2 * size });
    let low = accept(&mut pool, &chain, mk(1, 300)).unwrap();
    accept(&mut pool, &chain, mk(2, 400)).unwrap();
    // Pleine : une transaction mieux payée évince la moins rémunératrice…
    accept(&mut pool, &chain, mk(3, 1000)).unwrap();
    assert!(!pool.contains(&low));
    assert_eq!(pool.len(), 2);
    // … mais une moins bien payée est refusée.
    assert_eq!(accept(&mut pool, &chain, mk(4, 250)), Err(MempoolError::MempoolFull));
    assert_eq!(pool.total_bytes(), 2 * size);
}

/// Après la fin de l'émission, un bloc sans frais doit rester minable : sa
/// récompense n'a alors aucune sortie (une sortie de montant nul est interdite).
#[test]
fn blocks_remain_valid_after_emission_ends() {
    // Mêmes paramètres que regtest (donc même genèse), mais une division par
    // deux à chaque bloc : la subvention tombe à 0 au bloc 33.
    let mut p = Network::Regtest.params().clone();
    p.halving_interval = 1;
    let params: &'static ConsensusParams = Box::leak(Box::new(p));
    let mut chain = Chain::new(params, MemoryStore::default()).unwrap();
    let pool = Mempool::new(MempoolPolicy::default());
    for _ in 0..40 {
        let now = chain.tip().header.timestamp + 1;
        let mut block = create_block_template(&chain, &pool, pkh(1), now, b"").block;
        mine_header(&mut block.header, 1_000_000).unwrap();
        let reward_is_zero = params.block_subsidy(chain.height() + 1) == 0;
        assert_eq!(block.transactions[0].outputs.is_empty(), reward_is_zero);
        extend(&mut chain, block);
    }
    assert_eq!(params.block_subsidy(40), 0);
    assert_eq!(chain.height(), 40);
    chain.check_consistency().unwrap();

    // Un transfert sans sortie reste interdit.
    let tx = kynovara_core::tx::Transaction::transfer(vec![], vec![]);
    assert_eq!(kynovara_core::validation::check_tx_structure(&tx), Err(TxError::NoOutputs));
}

/// Paramètres de test avec ajustement de difficulté actif et minage rapide.
fn retarget_params() -> &'static ConsensusParams {
    fn build(nonce: u64, hash: Option<Hash256>) -> ConsensusParams {
        let base = Network::Regtest.params();
        ConsensusParams {
            network: Network::Regtest,
            hrp: base.hrp,
            magic: base.magic,
            default_p2p_port: base.default_p2p_port,
            default_rpc_port: base.default_rpc_port,
            pow_limit: U256::low_bits(255),
            genesis: GenesisParams {
                timestamp: 1_000_000,
                target: U256::low_bits(250),
                nonce,
                message: "retarget",
                hash,
            },
            target_spacing: 120,
            lwma_window: 10,
            no_retarget: false,
            max_future_block_time: 720,
            initial_subsidy: 50 * COIN,
            halving_interval: 150,
            coinbase_maturity: 100,
            slip44_coin_type: 1,
            checkpoint_master: None,
            checkpoint_sunset_height: 0,
            release_checkpoints: &[],
            transfers_unlock_height: 0,
            seeds: &[],
        }
    }
    let tmp: &'static ConsensusParams = Box::leak(Box::new(build(0, None)));
    let mut header = genesis_header(tmp, 0);
    mine_header(&mut header, 100_000_000).unwrap();
    Box::leak(Box::new(build(header.nonce, Some(header.hash()))))
}

#[test]
fn difficulty_rises_when_blocks_are_fast() {
    let params = retarget_params();
    let mut chain = Chain::new(params, MemoryStore::default()).unwrap();
    let initial = params.genesis.target;
    let push = |chain: &mut Chain<MemoryStore>, spacing: u64, tag: u8| {
        let mut block = make_block(chain, chain.tip_hash(), vec![], pkh(1), 0, tag);
        block.header.timestamp = chain.tip().header.timestamp + spacing;
        mine_header(&mut block.header, 100_000_000).unwrap();
        assert_eq!(Some(block.header.target), chain.next_target_after(&chain.tip_hash()));
        extend(chain, block);
        chain.tip().header.target.to_f64() / initial.to_f64()
    };
    // Tant que la fenêtre (10 blocs) n'est pas remplie : cible de la genèse.
    for i in 0..10u8 {
        assert_eq!(push(&mut chain, 30, i), 1.0);
    }
    // Blocs 4 fois trop rapides : la cible baisse (difficulté en hausse).
    let fast = push(&mut chain, 30, 10);
    assert!((fast - 0.25).abs() < 0.01, "{fast}");
    // Blocs 4 fois trop lents ensuite : la cible remonte.
    let mut last = fast;
    for i in 0..12u8 {
        last = push(&mut chain, 480, 20 + i);
    }
    assert!(last > fast * 4.0, "{fast} -> {last}");
    // Un bloc qui prétend à l'ancienne cible est refusé.
    let mut cheat = make_block(&chain, chain.tip_hash(), vec![], pkh(1), 0, 99);
    cheat.header.target = initial;
    mine_header(&mut cheat.header, 100_000_000).unwrap();
    assert!(matches!(submit(&mut chain, cheat), Err(kynovara_core::validation::BlockError::BadTarget { .. })));
}
