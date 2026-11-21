//! Relecture indépendante de la 0.4.0 : travail cumulé à travers le passage
//! de SHA-256 double à RandomX (constat V-1 d'AUDIT.md).
//!
//! Le travail d'un bloc est `2^256 / (cible + 1)`. Un bloc SHA-256 vaut donc
//! autant d'« unités de travail » que d'essais SHA-256 attendus, et un bloc
//! RandomX autant que d'essais RandomX : sur le testnet, environ 2^30 contre
//! 2^14 au moment de l'activation. Tant que les deux étaient additionnés, une
//! branche partant d'avant l'activation avec quelques blocs SHA-256 un peu
//! plus difficiles (horodatages rapprochés : LWMA relève la difficulté)
//! dépassait des milliers de blocs RandomX honnêtes. Depuis la correction,
//! les branches se comparent d'abord au travail RandomX (`ChainWork`) : ces
//! tests, d'abord écrits pour démontrer l'attaque, vérifient qu'elle échoue.
//!
//! Paramètres dédiés : SHA-256 avec ajustement LWMA (fenêtre de 4 blocs,
//! cible de départ 2^244, soit 4 096 essais par bloc) jusqu'au bloc 9,
//! RandomX à partir du bloc 10 (cible 2^255 : 2 unités de travail par bloc).
#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use kynovara_core::block::{BLOCK_VERSION, Block, BlockHeader};
use kynovara_core::chain::{Chain, CheckpointOutcome, MemoryStore, ProcessOutcome};
use kynovara_core::genesis::genesis_header;
use kynovara_core::hash::Hash256;
use kynovara_core::merkle::merkle_root;
use kynovara_core::miner::{mine_header, mine_header_randomx};
use kynovara_core::params::{ConsensusParams, GenesisParams, Network, PowAlgorithm, RandomXParams};
use kynovara_core::pow::RandomXHasher;
use kynovara_core::tx::{Transaction, TxOut};
use kynovara_core::u256::{U256, work_from_target};
use kynovara_core::validation::BlockError;

const ACTIVATION: u64 = 10;
/// Blocs RandomX honnêtes minés après l'activation.
const HONEST_RANDOMX_BLOCKS: u64 = 40;

fn params_with(randomx: Option<RandomXParams>) -> &'static ConsensusParams {
    fn build(randomx: Option<RandomXParams>, nonce: u64, hash: Option<Hash256>) -> ConsensusParams {
        let base = Network::Regtest.params();
        ConsensusParams {
            pow_limit: U256::low_bits(248),
            randomx,
            genesis: GenesisParams {
                timestamp: 1_000_000,
                target: U256::low_bits(244),
                nonce,
                message: "relecture bifurcation",
                hash,
            },
            target_spacing: 120,
            lwma_window: 4,
            no_retarget: false,
            max_future_block_time: 720,
            ..base.clone()
        }
    }
    let tmp: &'static ConsensusParams = Box::leak(Box::new(build(randomx, 0, None)));
    let mut header = genesis_header(tmp, 0);
    mine_header(&mut header, 100_000_000).unwrap();
    Box::leak(Box::new(build(randomx, header.nonce, Some(header.hash()))))
}

fn rx() -> RandomXParams {
    RandomXParams {
        activation_height: ACTIVATION,
        pow_limit: U256::low_bits(255),
        initial_target: U256::low_bits(255),
        key_interval: 2048,
        key_lag: 64,
    }
}

/// Paramètres « 0.4.0 » (RandomX au bloc 10).
fn fork_params() -> &'static ConsensusParams {
    params_with(Some(rx()))
}

#[derive(Default)]
struct Miner {
    hasher: Option<RandomXHasher>,
}

impl Miner {
    /// Enfant de `parent` daté `spacing` secondes après lui, miné avec
    /// l'algorithme exigé à sa hauteur.
    fn child(&mut self, chain: &Chain<MemoryStore>, parent: Hash256, tag: u8, spacing: u64) -> Block {
        let entry = chain.entry(&parent).unwrap();
        let height = entry.height + 1;
        let reward = chain.params().block_subsidy(height);
        let coinbase = Transaction::coinbase(height, vec![tag], vec![TxOut { value: reward, recipient: pkh(1) }]);
        let mut header = BlockHeader {
            version: BLOCK_VERSION,
            prev_hash: parent,
            merkle_root: merkle_root(&[coinbase.wtxid()]),
            timestamp: entry.header.timestamp + spacing,
            target: chain.next_target_after(&parent).unwrap(),
            nonce: 0,
        };
        match chain.randomx_key_for_child(&parent) {
            None => {
                mine_header(&mut header, 100_000_000).unwrap();
            }
            Some(key) => {
                if self.hasher.as_ref().map(|h| *h.key()) != Some(key) {
                    self.hasher = None;
                    self.hasher = Some(RandomXHasher::new(&key).unwrap());
                }
                mine_header_randomx(self.hasher.as_ref().unwrap(), &mut header, 10_000).unwrap().unwrap();
            }
        }
        Block { header, transactions: vec![coinbase] }
    }
}

/// Chaîne honnête : blocs toutes les 2 minutes, SHA-256 jusqu'au bloc 9 puis
/// `HONEST_RANDOMX_BLOCKS` blocs RandomX.
fn honest_chain(miner: &mut Miner) -> Chain<MemoryStore> {
    let params = fork_params();
    let mut chain = Chain::new(params, MemoryStore::default()).unwrap();
    for _ in 0..ACTIVATION - 1 + HONEST_RANDOMX_BLOCKS {
        let block = miner.child(&chain, chain.tip_hash(), 1, params.target_spacing);
        extend(&mut chain, block);
    }
    assert_eq!(params.pow_algorithm(chain.height()), PowAlgorithm::RandomX);
    chain
}

#[test]
fn relecture_deux_blocs_sha_d_avant_l_activation_n_effacent_plus_les_blocs_randomx() {
    let mut miner = Miner::default();
    let mut chain = honest_chain(&mut miner);
    let honest_height = chain.height();
    let honest_tip = chain.tip_hash();
    let honest_work = chain.tip().chain_work;
    assert_eq!(honest_height, ACTIVATION - 1 + HONEST_RANDOMX_BLOCKS);

    // L'attaquant part du bloc 7 (SHA-256) et mine deux blocs SHA-256 datés à
    // une seconde d'intervalle : le second a une cible plus basse (LWMA).
    let fork = chain.block_hash_at(ACTIVATION - 3).unwrap();
    let a8 = miner.child(&chain, fork, 2, 1);
    assert!(matches!(submit(&mut chain, a8.clone()).unwrap(), ProcessOutcome::Accepted { tip_change: None, .. }));
    assert_eq!(chain.randomx_key_for_child(&a8.hash()), None, "le bloc 9 reste en SHA-256");
    let a9 = miner.child(&chain, a8.hash(), 2, 1);
    let honest9 = chain.entry(&chain.block_hash_at(ACTIVATION - 1).unwrap()).unwrap().header.target;
    assert!(a9.header.target < honest9);
    let extra = work_from_target(a9.header.target).to_f64() - work_from_target(honest9).to_f64();
    let rx_block_work = work_from_target(rx().initial_target).to_f64();
    eprintln!(
        "travail : bloc SHA honnête {:.0}, bloc SHA de l'attaquant {:.0}, bloc RandomX {:.0} ; \
         surplus de l'attaquant ≈ {:.0} blocs RandomX",
        work_from_target(honest9).to_f64(),
        work_from_target(a9.header.target).to_f64(),
        rx_block_work,
        extra / rx_block_work
    );

    // Avant la correction, la chaîne active redescendait ici au bloc 9, sans
    // aucun bloc RandomX. Désormais, la branche de l'attaquant (aucun travail
    // RandomX) reste une branche secondaire, malgré son travail SHA-256.
    assert!(matches!(submit(&mut chain, a9.clone()).unwrap(), ProcessOutcome::Accepted { tip_change: None, .. }));
    assert_eq!(chain.tip_hash(), honest_tip);
    assert_eq!(chain.height(), honest_height);
    let attacker = chain.entry(&a9.hash()).unwrap().chain_work;
    assert!(attacker.sha256d > honest_work.sha256d, "plus de travail SHA-256…");
    assert!(attacker < honest_work, "…mais moins de travail RandomX");

    // Un bloc RandomX sur la branche de l'attaquant ne suffit pas non plus :
    // il lui faudrait plus de travail RandomX que les 40 blocs honnêtes.
    let a10 = miner.child(&chain, a9.hash(), 2, 120);
    assert!(matches!(submit(&mut chain, a10).unwrap(), ProcessOutcome::Accepted { tip_change: None, .. }));
    assert_eq!(chain.tip_hash(), honest_tip);
    assert!(chain.is_active(&honest_tip));
    chain.check_consistency().unwrap();
}

#[test]
fn relecture_seul_un_point_de_controle_randomx_empeche_cette_reecriture() {
    let mut miner = Miner::default();
    let mut chain = honest_chain(&mut miner);
    // Point de contrôle (signé, ou inscrit dans la version) sur un bloc RandomX.
    let cp_height = ACTIVATION + 2;
    let cp = chain.block_hash_at(cp_height).unwrap();
    assert!(matches!(chain.apply_checkpoint(cp_height, cp, false), CheckpointOutcome::Applied { .. }));
    let fork = chain.block_hash_at(ACTIVATION - 3).unwrap();
    let a8 = miner.child(&chain, fork, 2, 1);
    let err = submit(&mut chain, a8).unwrap_err();
    assert_eq!(err, BlockError::ConflictsWithCheckpoint { height: cp_height });
    // Refusé sans pénalité pour le pair (règle de choix, pas de validité).
    assert!(!err.is_peer_fault());
}

#[test]
fn relecture_echelle_du_testnet() {
    // Pourquoi les deux travaux ne doivent jamais s'additionner : même calcul
    // avec les vrais paramètres du testnet (`next_target`), en supposant des
    // blocs SHA-256 à la cible de la genèse (2^226, ≈ 2^30 essais ; davantage
    // si la difficulté a monté depuis) : l'attaquant part du bloc 15 989 et
    // mine dix blocs SHA-256 datés à une seconde d'intervalle. Additionné, ce
    // surplus vaudrait des mois de blocs RandomX ; avec `ChainWork`, il ne
    // compte qu'à travail RandomX égal.
    let p = &kynovara_core::params::TESTNET;
    let rx = p.randomx.unwrap();
    let n = p.lwma_window as usize;
    let honest = p.genesis.target;
    let mut window: Vec<(u64, U256)> = (0..=n as u64).map(|i| (1_800_000_000 + 120 * i, honest)).collect();
    let rx_work = work_from_target(rx.initial_target).to_f64();
    let mut surplus = 0.0;
    let mut by_count = Vec::new();
    for parent in rx.activation_height - 11..rx.activation_height - 1 {
        let target = kynovara_core::difficulty::next_target(p, parent, &window);
        assert_eq!(p.pow_algorithm(parent + 1), PowAlgorithm::Sha256d);
        surplus += work_from_target(target).to_f64() - work_from_target(honest).to_f64();
        by_count.push(surplus / rx_work);
        let last = window.last().unwrap().0;
        window.remove(0);
        window.push((last + 1, target));
    }
    eprintln!("surplus de l'attaquant en blocs RandomX (2^242) après 1 à 10 blocs SHA-256 : {by_count:.0?}");
    assert!(by_count[1] > 2_000.0, "{by_count:?}");
    assert!(by_count[9] > 100_000.0, "{by_count:?}");
}

#[test]
fn relecture_blocs_sha_ecrits_par_une_0_3_apres_l_activation_refuses_a_la_relecture() {
    // Confirme une affirmation de l'AUDIT : un nœud « 0.3.x » (mêmes règles
    // sans RandomX) qui a continué la chaîne SHA-256 après le bloc 10 a écrit
    // des blocs que la relecture « de confiance » de la 0.4.0 refuse (cible).
    let old = params_with(None);
    let new = fork_params();
    assert_eq!(old.genesis.hash, new.genesis.hash);
    let mut old_chain = Chain::new(old, MemoryStore::default()).unwrap();
    let mut miner = Miner::default();
    let mut blocks = Vec::new();
    for _ in 0..ACTIVATION + 5 {
        let block = miner.child(&old_chain, old_chain.tip_hash(), 1, 120);
        blocks.push(block.clone());
        extend(&mut old_chain, block);
    }
    let mut chain = Chain::new(new, MemoryStore::default()).unwrap();
    for (i, block) in blocks.into_iter().enumerate() {
        let height = i as u64 + 1;
        let result = chain.process_stored_block(block, u64::MAX / 2);
        if height < ACTIVATION {
            assert!(result.is_ok(), "bloc {height}");
        } else if height == ACTIVATION {
            assert!(matches!(result, Err(BlockError::BadTarget { .. })), "bloc {height} : {result:?}");
        } else {
            assert!(matches!(result, Ok(ProcessOutcome::Orphan { .. })), "bloc {height} : {result:?}");
        }
    }
    assert_eq!(chain.height(), ACTIVATION - 1);
}
