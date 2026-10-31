//! Pannes de stockage simulées : la chaîne doit rester cohérente et s'arrêter
//! net plutôt que de continuer avec un état faux.
//!
//! Non-régression d'un constat de l'audit (L-1) : une erreur de lecture pendant
//! la reconstruction de l'état (réorganisation plus profonde que les données
//! d'annulation) laissait l'ensemble UTXO à moitié reconstruit, une pièce
//! dépensée y réapparaissait et la mempool acceptait sa double dépense.
#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use kynovara_core::block::Block;
use kynovara_core::chain::{BlockStore, Chain, MemoryStore};
use kynovara_core::hash::Hash256;
use kynovara_core::mempool::{Mempool, MempoolError, MempoolPolicy};
use kynovara_core::tx::TxOut;
use kynovara_core::validation::{BlockError, TxError};
use std::sync::Mutex;

/// Stockage en mémoire dont la lecture d'un bloc choisi échoue.
#[derive(Default)]
struct FlakyStore {
    inner: MemoryStore,
    fail_on: Mutex<Option<Hash256>>,
}

impl BlockStore for FlakyStore {
    fn put(&mut self, hash: &Hash256, block: &Block) -> Result<(), String> {
        self.inner.put(hash, block)
    }
    fn get(&self, hash: &Hash256) -> Result<Option<Block>, String> {
        if *self.fail_on.lock().unwrap() == Some(*hash) {
            return Err("erreur d'entrée-sortie simulée".into());
        }
        self.inner.get(hash)
    }
    fn contains(&self, hash: &Hash256) -> bool {
        self.inner.contains(hash)
    }
}

#[test]
fn failed_rebuild_keeps_state_consistent_and_stops_the_chain() {
    let mut chain = Chain::new(regtest(), FlakyStore::default()).unwrap();
    chain.set_undo_depth(2);
    let g = chain.genesis_hash();
    mine_empty(&mut chain, 101, pkh(1));
    let c1 = coinbase_output(&chain, 1);
    let fork = chain.tip_hash(); // hauteur 101
    // Bloc 102 : la pièce C1 est dépensée.
    let t = spend(&g, &key(1), &[c1], vec![TxOut { value: c1.1.value - 1000, recipient: pkh(2) }]);
    let b = make_block(&chain, fork, vec![t], pkh(1), 0, 0);
    extend(&mut chain, b);
    mine_empty(&mut chain, 4, pkh(1)); // sommet : 106
    assert!(chain.utxo().get(&c1.0).is_none());

    // La lecture du bloc 50 échouera pendant la reconstruction.
    let h50 = chain.block_hash_at(50).unwrap();
    *chain.store().fail_on.lock().unwrap() = Some(h50);

    // Branche concurrente plus lourde depuis 101, sans dépense de C1 : la
    // bascule exige une reconstruction complète, qui échoue.
    let mut parent = fork;
    for i in 0..6u8 {
        let b = make_block(&chain, parent, vec![], pkh(9), 0, 100 + i);
        parent = b.hash();
        let _ = submit(&mut chain, b);
    }
    assert!(chain.fatal_error().is_some(), "l'erreur de stockage doit arrêter la chaîne");

    // L'état reste exactement celui de la chaîne active (aucun état à moitié
    // reconstruit) : C1, dépensée au bloc 102 toujours actif, le reste.
    *chain.store().fail_on.lock().unwrap() = None;
    chain.check_consistency().unwrap();
    assert!(chain.height() >= 102);
    assert!(chain.utxo().get(&c1.0).is_none(), "une pièce dépensée ne doit jamais réapparaître");
    let mut pool = Mempool::new(MempoolPolicy::default());
    let double = spend(&g, &key(1), &[c1], vec![TxOut { value: c1.1.value - 100_000, recipient: pkh(3) }]);
    let r = pool.accept(double, chain.utxo(), chain.height() + 1, chain.params(), &g);
    assert!(matches!(r, Err(MempoolError::Invalid(TxError::MissingInput(_)))), "{r:?}");

    // Plus aucun bloc n'est accepté tant que le nœud n'a pas redémarré.
    let next = make_block(&chain, chain.tip_hash(), vec![], pkh(1), 0, 200);
    assert!(matches!(submit(&mut chain, next), Err(BlockError::Storage(_))));
}
