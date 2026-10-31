//! Test différentiel de la chaîne (écrit lors de la relecture indépendante).
//!
//! Des branches aléatoires contiennent des blocs invalides selon le contexte
//! (une dépense valide sur une branche, double dépense sur une autre), avec une
//! profondeur d'annulation minuscule (reconstructions complètes fréquentes),
//! une réserve en mémoire minuscule et des comptes rendus tronqués. Après
//! chaque bloc, l'état est comparé à un rejeu indépendant depuis la genèse :
//! - aucun bloc valide n'est jamais marqué invalide (et inversement) ;
//! - la chaîne active est la meilleure chaîne valide ;
//! - l'ensemble UTXO correspond au rejeu.
#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use kynovara_core::block::Block;
use kynovara_core::chain::BlockStatus;
use kynovara_core::hash::Hash256;
use kynovara_core::tx::{OutPoint, Transaction, TxOut};
use kynovara_core::utxo::UtxoSet;
use kynovara_core::validation::connect_block;
use std::collections::HashMap;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Validité de référence : rejeu depuis la genèse avec un ensemble UTXO neuf.
fn reference_valid(blocks: &HashMap<Hash256, Block>, genesis: Hash256, hash: Hash256) -> bool {
    let mut path = Vec::new();
    let mut cur = hash;
    while cur != genesis {
        let b = &blocks[&cur];
        path.push(cur);
        cur = b.header.prev_hash;
    }
    path.reverse();
    let mut utxo = UtxoSet::new();
    path.iter()
        .enumerate()
        .all(|(i, h)| connect_block(&mut utxo, &blocks[h], i as u64 + 1, regtest(), &genesis).is_ok())
}

#[test]
fn differential_random_reorgs_with_context_dependent_invalid_blocks() {
    for seed in 1..=10u64 {
        let mut rng = Rng(seed);
        let mut chain = new_chain();
        chain.set_undo_depth(1 + rng.below(3));
        // Marge de branche variée : tout sur le disque, ou beaucoup en mémoire.
        chain.set_side_branch_margin([0, 2, 1_000][rng.below(3) as usize]);
        // Réserve en mémoire minuscule et compte rendu tronqué : chemins d'éviction.
        if seed % 2 == 0 {
            chain.set_parked_limits(3 + rng.below(4) as usize, 1 << 20);
            chain.set_tip_change_limits(2, 1 << 20);
        }
        let g = chain.genesis_hash();
        let mut blocks: HashMap<Hash256, Block> = HashMap::new();
        for b in mine_empty(&mut chain, 105, pkh(1)) {
            blocks.insert(b.hash(), b);
        }
        // Pièces communes à toutes les branches (hauteurs 1 à 6, mûres partout).
        let coins: Vec<(OutPoint, TxOut)> = (1..=6).map(|h| coinbase_output(&chain, h)).collect();
        let mut tips: Vec<Hash256> = vec![chain.tip_hash()];
        let mut submitted = 0;
        // La validité d'un bloc ne dépend que de ses ancêtres : calculée une fois.
        let mut validity: HashMap<Hash256, bool> = HashMap::new();
        for round in 0..140u32 {
            let mut parent = tips[rng.below(tips.len() as u64) as usize];
            if rng.below(3) == 0 {
                let h = chain.entry(&parent).unwrap().height;
                let back = rng.below(6);
                if h > 105 + back
                    && let Some(a) = chain.ancestor(&parent, h - back)
                {
                    parent = a;
                }
            }
            if chain.entry(&parent).map(|e| e.status) != Some(BlockStatus::Valid)
                && chain.entry(&parent).map(|e| e.status) != Some(BlockStatus::Pending)
            {
                continue;
            }
            let mut txs: Vec<Transaction> = Vec::new();
            match rng.below(6) {
                0 | 1 => {
                    // Dépense d'une pièce commune (valide si pas déjà dépensée sur cette branche).
                    let c = coins[rng.below(coins.len() as u64) as usize];
                    let fee = rng.below(3);
                    txs.push(spend(&g, &key(1), &[c], vec![TxOut { value: c.1.value - fee, recipient: pkh(2) }]));
                }
                2 => {
                    // Chaîne de dépenses dans le même bloc.
                    let c = coins[rng.below(coins.len() as u64) as usize];
                    let t1 = spend(&g, &key(1), &[c], vec![TxOut { value: c.1.value - 7, recipient: pkh(2) }]);
                    let o = (OutPoint { txid: t1.txid(), vout: 0 }, t1.outputs[0]);
                    let t2 = spend(&g, &key(2), &[o], vec![TxOut { value: o.1.value, recipient: pkh(3) }]);
                    txs.push(t1);
                    txs.push(t2);
                }
                3 => {
                    // Double dépense dans le même bloc (toujours invalide).
                    let c = coins[rng.below(coins.len() as u64) as usize];
                    txs.push(spend(&g, &key(1), &[c], vec![TxOut { value: c.1.value - 1, recipient: pkh(2) }]));
                    txs.push(spend(&g, &key(1), &[c], vec![TxOut { value: c.1.value - 2, recipient: pkh(3) }]));
                }
                _ => {}
            }
            let b = make_block(&chain, parent, txs, pkh(1), 0, (round % 251) as u8);
            let h = b.hash();
            blocks.insert(h, b.clone());
            let _ = submit(&mut chain, b);
            submitted += 1;
            if chain.entry(&h).is_some_and(|e| e.status != BlockStatus::Invalid) {
                tips.retain(|t| *t != parent);
                tips.push(h);
            }
            tips.retain(|t| chain.entry(t).is_some_and(|e| e.status != BlockStatus::Invalid));
            if tips.is_empty() {
                tips.push(chain.tip_hash());
            }

            // Invariants.
            chain.check_consistency().unwrap();
            let mut best_valid_height = 0;
            for bh in blocks.keys() {
                let Some(e) = chain.entry(bh) else { continue };
                let ok = *validity.entry(*bh).or_insert_with(|| reference_valid(&blocks, g, *bh));
                match e.status {
                    BlockStatus::Valid => assert!(ok, "graine {seed} : bloc marqué valide mais invalide en référence"),
                    BlockStatus::Invalid => {
                        assert!(!ok, "graine {seed}, tour {round} : bloc VALIDE marqué invalide (h {})", e.height)
                    }
                    BlockStatus::Pending => {}
                }
                if ok {
                    best_valid_height = best_valid_height.max(e.height);
                }
            }
            assert_eq!(
                chain.height(),
                best_valid_height,
                "graine {seed}, tour {round} : sommet hors de la meilleure chaîne valide"
            );
        }
        assert!(submitted > 100);
    }
}
