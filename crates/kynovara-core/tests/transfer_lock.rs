//! Période de rodage : avant la hauteur fixée, on mine et on accumule, mais
//! aucun transfert n'est accepté (ni en mempool, ni dans un bloc, ni dans le
//! modèle du mineur) ; à partir de cette hauteur, tout redevient normal.
#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use kynovara_core::chain::{Chain, MemoryStore};
use kynovara_core::mempool::{Mempool, MempoolError, MempoolPolicy};
use kynovara_core::miner::create_block_template;
use kynovara_core::params::ConsensusParams;
use kynovara_core::tx::TxOut;
use kynovara_core::validation::{BlockError, connect_block};

fn locked_until(unlock: u64) -> &'static ConsensusParams {
    Box::leak(Box::new(ConsensusParams { transfers_unlock_height: unlock, ..regtest().clone() }))
}

#[test]
fn no_transfer_before_the_unlock_height_then_normal() {
    let params = locked_until(110);
    let mut chain = Chain::new(params, MemoryStore::default()).unwrap();
    mine_empty(&mut chain, 101, pkh(1));
    let g = chain.genesis_hash();
    let coin = coinbase_output(&chain, 1);
    let tx = spend(&g, &key(1), &[coin], vec![TxOut { value: coin.1.value - 1_000, recipient: pkh(2) }]);

    // Mempool : refus clair, sans rien retenir.
    let mut pool = Mempool::new(MempoolPolicy::default());
    let next = chain.height() + 1;
    assert_eq!(
        pool.accept(tx.clone(), chain.utxo(), next, params, &g).unwrap_err(),
        MempoolError::TransfersLocked { unlock_height: 110 }
    );
    assert!(!MempoolError::TransfersLocked { unlock_height: 110 }.is_invalid_everywhere(), "pas de bannissement");

    // Bloc contenant un transfert : refusé avant d'être stocké.
    let bad = make_block(&chain, chain.tip_hash(), vec![tx.clone()], pkh(1), 1_000, 1);
    let bad_hash = bad.hash();
    assert_eq!(
        submit(&mut chain, bad.clone()).unwrap_err(),
        BlockError::TransfersLocked { height: 102, unlock_height: 110 }
    );
    assert!(chain.entry(&bad_hash).is_none());
    // La règle tient aussi lors d'un rejeu complet (validation de consensus).
    let mut utxo = chain.utxo().clone();
    assert!(matches!(connect_block(&mut utxo, &bad, 102, params, &g), Err(BlockError::TransfersLocked { .. })));

    // Le minage continue normalement pendant le rodage.
    mine_empty(&mut chain, 8, pkh(1));
    assert_eq!(chain.height(), 109);

    // Au bloc 110, le même transfert passe partout.
    let next = chain.height() + 1;
    pool.accept(tx.clone(), chain.utxo(), next, params, &g).unwrap();
    let template = create_block_template(&chain, &pool, pkh(1), u64::MAX / 4, &[]);
    assert_eq!(template.block.transactions.len(), 2, "le mineur inclut le transfert");
    let good = make_block(&chain, chain.tip_hash(), vec![tx], pkh(1), 1_000, 2);
    extend(&mut chain, good);
    assert_eq!(chain.height(), 110);
    chain.check_consistency().unwrap();
}

#[test]
fn going_back_into_the_lock_drops_pending_transfers() {
    let params = locked_until(110);
    let mut chain = Chain::new(params, MemoryStore::default()).unwrap();
    mine_empty(&mut chain, 105, pkh(1));
    let g = chain.genesis_hash();
    let coin = coinbase_output(&chain, 1);
    let tx = spend(&g, &key(1), &[coin], vec![TxOut { value: coin.1.value - 1_000, recipient: pkh(2) }]);
    // Transfert accepté pour une hauteur sans verrou (cas d'une chaîne qui
    // aurait ensuite reculé sous la hauteur de fin du rodage).
    let mut pool = Mempool::new(MempoolPolicy::default());
    pool.accept(tx, chain.utxo(), 110, params, &g).unwrap();
    // Le modèle du mineur ne l'inclut pas tant que le rodage dure…
    let template = create_block_template(&chain, &pool, pkh(1), u64::MAX / 4, &[]);
    assert_eq!(template.block.transactions.len(), 1);
    // … et la revérification de la mempool le retire.
    pool.revalidate(chain.utxo(), chain.height() + 1, params);
    assert_eq!(pool.len(), 0);
}
