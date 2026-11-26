//! Relecture indépendante de la 0.4.0 : coût des orphelins RandomX (constat
//! V-3 d'AUDIT.md).
//!
//! Un orphelin RandomX « proche du sommet » est gardé sans preuve
//! (`may_keep_orphan`) : sa clé dépend de sa branche, encore inconnue. Quand
//! son parent arrive, il est vérifié (≈ 15 ms). Avant la correction, un pair
//! pouvait ainsi faire calculer jusqu'à 64 empreintes RandomX (la taille de la
//! réserve) à chaque bloc qu'il voyait arriver, sans jamais être banni. La
//! réserve garde désormais au plus `MAX_ORPHANS_PER_PARENT` orphelins par
//! parent manquant : ce test, d'abord écrit pour démontrer le problème,
//! vérifie la limite.
#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use kynovara_core::block::{BLOCK_VERSION, Block, BlockHeader, hash_meets_target};
use kynovara_core::chain::{Chain, MAX_ORPHANS_PER_PARENT, MemoryStore, ProcessOutcome};
use kynovara_core::hash::Hash256;
use kynovara_core::merkle::merkle_root;
use kynovara_core::miner::{mine_header, mine_header_randomx};
use kynovara_core::params::{ConsensusParams, RandomXParams};
use kynovara_core::pow::RandomXHasher;
use kynovara_core::tx::{Transaction, TxOut};
use kynovara_core::u256::U256;
use kynovara_core::validation::BlockError;

const ACTIVATION: u64 = 3;
/// Taille de la réserve d'orphelins (`MAX_ORPHANS` dans chain.rs).
const ORPHANS: usize = 64;

fn rx_params() -> &'static ConsensusParams {
    Box::leak(Box::new(ConsensusParams {
        randomx: Some(RandomXParams {
            activation_height: ACTIVATION,
            pow_limit: U256::low_bits(255),
            initial_target: U256::low_bits(255),
            // Une seule clé (la genèse) pendant tout le test.
            key_interval: 2048,
            key_lag: 64,
        }),
        ..regtest().clone()
    }))
}

fn unmined(chain: &Chain<MemoryStore>, parent: Hash256, tag: u8) -> Block {
    let entry = chain.entry(&parent).unwrap();
    let height = entry.height + 1;
    let coinbase = Transaction::coinbase(
        height,
        vec![tag, tag.wrapping_mul(7)],
        vec![TxOut { value: chain.params().block_subsidy(height), recipient: pkh(1) }],
    );
    let header = BlockHeader {
        version: BLOCK_VERSION,
        prev_hash: parent,
        merkle_root: merkle_root(&[coinbase.wtxid()]),
        timestamp: entry.header.timestamp + chain.params().target_spacing,
        target: chain.next_target_after(&parent).unwrap(),
        nonce: 0,
    };
    Block { header, transactions: vec![coinbase] }
}

fn mined(chain: &Chain<MemoryStore>, hasher: &RandomXHasher, parent: Hash256, tag: u8) -> Block {
    let mut block = unmined(chain, parent, tag);
    match chain.randomx_key_for_child(&parent) {
        None => {
            mine_header(&mut block.header, 10_000_000).unwrap();
        }
        Some(key) => {
            assert_eq!(&key, hasher.key());
            mine_header_randomx(hasher, &mut block.header, 10_000).unwrap().unwrap();
        }
    }
    block
}

/// Enfant de `parent` correct en tout (hauteur, cible, horodatage) sauf sa
/// preuve RandomX.
fn forged_child(chain: &Chain<MemoryStore>, hasher: &RandomXHasher, parent: Hash256, tag: u8) -> Block {
    let mut block = unmined(chain, parent, tag);
    loop {
        let hash = hasher.hash(&block.header.to_array()).unwrap();
        if !hash_meets_target(&hash, &block.header.target) {
            return block;
        }
        block.header.nonce += 1;
    }
}

#[test]
fn relecture_les_orphelins_randomx_faux_sont_limites_par_parent() {
    let params = rx_params();
    let genesis_key = Chain::new(params, MemoryStore::default()).unwrap().genesis_hash();
    let hasher = RandomXHasher::new(&genesis_key).unwrap();

    // Chaîne source jusqu'au bloc 6 (P) ; une victime qui connaît les blocs
    // 1 à 5 mais pas encore P.
    let mut source = Chain::new(params, MemoryStore::default()).unwrap();
    let mut blocks = Vec::new();
    for tag in 0..6u8 {
        let block = mined(&source, &hasher, source.tip_hash(), tag);
        blocks.push(block.clone());
        extend(&mut source, block);
    }
    let p = blocks.pop().unwrap();
    let mut victim = Chain::new(params, MemoryStore::default()).unwrap();
    for block in &blocks {
        extend(&mut victim, block.clone());
    }

    // 64 enfants de P à la preuve fausse, reçus avant P : tous « orphelins »
    // pour l'expéditeur, mais deux seulement sont gardés.
    let forged: Vec<Block> = (0..ORPHANS).map(|i| forged_child(&source, &hasher, p.hash(), 100 + i as u8)).collect();
    for block in &forged {
        assert!(matches!(submit(&mut victim, block.clone()).unwrap(), ProcessOutcome::Orphan { .. }));
    }
    assert_eq!(victim.orphan_count(), MAX_ORPHANS_PER_PARENT);

    // Le vrai enfant de P, arrivé après eux, n'a plus de place…
    let honest = mined(&source, &hasher, p.hash(), 7);
    assert!(matches!(submit(&mut victim, honest.clone()).unwrap(), ProcessOutcome::Orphan { .. }));
    assert_eq!(victim.orphan_count(), MAX_ORPHANS_PER_PARENT);

    // Arrivée de P : seuls les deux orphelins gardés sont vérifiés (et
    // écartés), au lieu de 64.
    match submit(&mut victim, p.clone()).unwrap() {
        ProcessOutcome::Accepted { tip_change: Some(_), invalid } => assert!(invalid.is_empty()),
        other => panic!("P aurait dû être accepté : {other:?}"),
    }
    assert_eq!(victim.tip_hash(), p.hash());
    assert_eq!(victim.orphan_count(), 0);
    assert!(forged.iter().all(|b| victim.entry(&b.hash()).is_none()));

    // …mais il est accepté quand la synchronisation le redemande.
    extend(&mut victim, honest);

    // Un bloc faux envoyé une fois son parent connu : refusé, faute du pair.
    let err = submit(&mut victim, forged[0].clone()).unwrap_err();
    assert_eq!(err, BlockError::InsufficientWork);
    assert!(err.is_peer_fault());
}
