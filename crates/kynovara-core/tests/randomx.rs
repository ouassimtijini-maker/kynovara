//! Preuve de travail RandomX dans la chaîne : activation à une hauteur,
//! calendrier des clés (qui dépend de la branche), relecture du stockage et
//! orphelins.
//!
//! Paramètres dédiés : regtest dont les blocs passent en RandomX au bloc 3,
//! avec une clé qui change tous les 4 blocs (décalage 1), et une cible si
//! facile qu'un essai sur deux réussit.
#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use kynovara_core::block::{BLOCK_VERSION, Block, BlockHeader, hash_meets_target};
use kynovara_core::chain::{Chain, MemoryStore, ProcessOutcome};
use kynovara_core::genesis::genesis_header;
use kynovara_core::hash::Hash256;
use kynovara_core::mempool::Mempool;
use kynovara_core::merkle::merkle_root;
use kynovara_core::miner::{create_block_template, mine_header, mine_header_randomx};
use kynovara_core::params::{ConsensusParams, GenesisParams, PowAlgorithm, RandomXParams};
use kynovara_core::pow::{GENESIS_RANDOMX_KEY, RandomXHasher, randomx_meets_target};
use kynovara_core::tx::{Transaction, TxOut};
use kynovara_core::u256::U256;
use kynovara_core::validation::{BlockError, check_block_structure};

const ACTIVATION: u64 = 3;

fn rx_params_with(pow_limit: U256) -> &'static ConsensusParams {
    Box::leak(Box::new(ConsensusParams {
        randomx: Some(RandomXParams {
            activation_height: ACTIVATION,
            pow_limit,
            initial_target: pow_limit,
            key_interval: 4,
            key_lag: 1,
        }),
        ..regtest().clone()
    }))
}

fn rx_params() -> &'static ConsensusParams {
    rx_params_with(U256::low_bits(255))
}

fn rx_chain() -> Chain<MemoryStore> {
    Chain::new(rx_params(), MemoryStore::default()).unwrap()
}

/// Bloc enfant de `parent`, pas encore miné.
fn unmined_block(chain: &Chain<MemoryStore>, parent: Hash256, tag: u8) -> Block {
    let entry = chain.entry(&parent).expect("parent connu");
    let height = entry.height + 1;
    let reward = chain.params().block_subsidy(height);
    let coinbase = Transaction::coinbase(height, vec![tag], vec![TxOut { value: reward, recipient: pkh(1) }]);
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

/// Premier nonce qui satisfait `accept`.
fn with_nonce(mut block: Block, accept: impl Fn(&BlockHeader) -> bool) -> Block {
    let mut nonce = 0u64;
    loop {
        block.header.nonce = nonce;
        if accept(&block.header) {
            return block;
        }
        nonce += 1;
    }
}

fn randomx_ok(hasher: &RandomXHasher, header: &BlockHeader) -> bool {
    hash_meets_target(&hasher.hash(&header.to_array()).unwrap(), &header.target)
}

/// Mineur de test : garde un seul calculateur RandomX (256 Mio), recréé
/// quand la clé change.
#[derive(Default)]
struct Miner {
    hasher: Option<RandomXHasher>,
}

impl Miner {
    fn hasher(&mut self, key: Hash256) -> &RandomXHasher {
        if self.hasher.as_ref().map(|h| *h.key()) != Some(key) {
            self.hasher = None;
            self.hasher = Some(RandomXHasher::new(&key).unwrap());
        }
        self.hasher.as_ref().unwrap()
    }

    /// Mine un enfant de `parent` avec l'algorithme (et la clé) exigés.
    fn mine(&mut self, chain: &Chain<MemoryStore>, parent: Hash256, tag: u8) -> Block {
        let mut block = unmined_block(chain, parent, tag);
        match chain.randomx_key_for_child(&parent) {
            None => {
                mine_header(&mut block.header, 10_000_000).unwrap();
            }
            Some(key) => {
                mine_header_randomx(self.hasher(key), &mut block.header, 10_000).unwrap().unwrap();
            }
        }
        block
    }

    /// Mine `n` blocs au sommet.
    fn extend_by(&mut self, chain: &mut Chain<MemoryStore>, n: usize, tag: u8) -> Vec<Block> {
        let mut out = Vec::new();
        for _ in 0..n {
            let block = self.mine(chain, chain.tip_hash(), tag);
            out.push(block.clone());
            extend(chain, block);
        }
        out
    }

    /// Enfant de `parent` dont la preuve SHA-256 double est valide mais pas la
    /// preuve RandomX avec la clé `key`.
    fn sha_only(&mut self, chain: &Chain<MemoryStore>, parent: Hash256, tag: u8, key: Hash256) -> Block {
        let hasher = self.hasher(key);
        with_nonce(unmined_block(chain, parent, tag), |h| h.meets_target() && !randomx_ok(hasher, h))
    }
}

#[test]
fn blocks_switch_to_randomx_at_the_activation_height() {
    let params = rx_params();
    let mut chain = rx_chain();
    let mut miner = Miner::default();
    let genesis = chain.genesis_hash();
    assert_eq!(params.pow_algorithm(ACTIVATION - 1), PowAlgorithm::Sha256d);
    assert_eq!(params.pow_algorithm(ACTIVATION), PowAlgorithm::RandomX);

    // Avant l'activation, seule la preuve SHA-256 double compte : un bloc
    // qui échouerait en RandomX est accepté.
    assert_eq!(chain.randomx_key_for_child(&genesis), None);
    let b1 = miner.sha_only(&chain, genesis, 1, genesis);
    extend(&mut chain, b1);
    miner.extend_by(&mut chain, 1, 2);
    assert_eq!(chain.height(), ACTIVATION - 1);

    // Le bloc 3 est le premier en RandomX ; sa clé est la genèse.
    let tip = chain.tip_hash();
    assert_eq!(chain.randomx_key_for_child(&tip), Some(genesis));
    let template = create_block_template(&chain, &Mempool::default(), pkh(1), 0, b"");
    assert_eq!(template.randomx_key, Some(genesis));
    assert_eq!(template.block.header.target, params.pow_limit_at(ACTIVATION));

    // Une preuve SHA-256 double ne suffit plus.
    let cheat = miner.sha_only(&chain, tip, 3, genesis);
    assert_eq!(submit(&mut chain, cheat).unwrap_err(), BlockError::InsufficientWork);
    assert_eq!(chain.height(), ACTIVATION - 1);

    // Le vrai bloc RandomX est accepté, et la chaîne continue.
    miner.extend_by(&mut chain, 3, 4);
    assert_eq!(chain.height(), ACTIVATION + 2);
    chain.check_consistency().unwrap();
}

#[test]
fn randomx_key_rotates_and_belongs_to_the_branch() {
    let mut chain = rx_chain();
    let mut miner = Miner::default();
    // Chaîne A jusqu'au bloc 10. Clés : genèse jusqu'au bloc 5, bloc 4 pour
    // les blocs 6 à 9, bloc 8 pour les blocs 10 à 13.
    let a = miner.extend_by(&mut chain, 10, 1);
    let at = |h: u64| a[(h - 1) as usize].hash();
    let genesis = chain.genesis_hash();
    assert_eq!(chain.randomx_key_for_child(&at(4)), Some(genesis));
    assert_eq!(chain.randomx_key_for_child(&at(5)), Some(at(4)));
    assert_eq!(chain.randomx_key_for_child(&at(8)), Some(at(4)));
    assert_eq!(chain.randomx_key_for_child(&at(9)), Some(at(8)));
    assert_eq!(chain.randomx_key_for_child(&at(10)), Some(at(8)));

    // Branche B partant du bloc 3 : son propre bloc 4 fournit sa clé.
    let mut side = |chain: &mut Chain<MemoryStore>, parent: Hash256| {
        let block = miner.mine(chain, parent, 2);
        assert!(matches!(submit(chain, block.clone()).unwrap(), ProcessOutcome::Accepted { tip_change: None, .. }));
        block
    };
    let b4 = side(&mut chain, at(3));
    let b5 = side(&mut chain, b4.hash());
    assert_eq!(chain.randomx_key_for_child(&b5.hash()), Some(b4.hash()));

    // Un bloc 6 de B miné avec la clé de A (et invalide avec celle de B) est refusé.
    let key_a = RandomXHasher::new(&at(4)).unwrap();
    let key_b = RandomXHasher::new(&b4.hash()).unwrap();
    let wrong = with_nonce(unmined_block(&chain, b5.hash(), 3), |h| randomx_ok(&key_a, h) && !randomx_ok(&key_b, h));
    assert_eq!(submit(&mut chain, wrong).unwrap_err(), BlockError::InsufficientWork);
    drop((key_a, key_b));

    // B miné avec ses propres clés finit par dépasser A : réorganisation.
    let mut parent = b5.hash();
    for _ in 6..=11 {
        let block = miner.mine(&chain, parent, 4);
        parent = block.hash();
        submit(&mut chain, block).unwrap();
    }
    assert_eq!(chain.tip_hash(), parent);
    assert_eq!(chain.height(), 11);
    assert_eq!(chain.block_hash_at(4), Some(b4.hash()));
    chain.check_consistency().unwrap();
}

#[test]
fn stored_blocks_skip_only_the_randomx_check() {
    let mut chain = rx_chain();
    let mut miner = Miner::default();
    miner.extend_by(&mut chain, 2, 1);
    let tip = chain.tip_hash();
    let genesis = chain.genesis_hash();
    let forged = miner.sha_only(&chain, tip, 2, genesis);

    // Reçu d'un pair : refusé.
    assert_eq!(submit(&mut chain, forged.clone()).unwrap_err(), BlockError::InsufficientWork);
    // Relu depuis notre propre stockage : la preuve RandomX n'est pas
    // recalculée (elle l'a été avant l'écriture)…
    let mut wrong_target = forged.clone();
    wrong_target.header.target = U256::low_bits(254);
    let wrong_target = with_nonce(wrong_target, BlockHeader::meets_target);
    assert!(matches!(chain.process_stored_block(wrong_target, u64::MAX / 2), Err(BlockError::BadTarget { .. })));
    // …mais toutes les autres règles le sont (ci-dessus), et seulement pendant
    // la relecture.
    assert!(matches!(
        chain.process_stored_block(forged.clone(), u64::MAX / 2).unwrap(),
        ProcessOutcome::Accepted { tip_change: Some(_), .. }
    ));
    assert_eq!(chain.tip_hash(), forged.hash());
    let next = miner.sha_only(&chain, forged.hash(), 3, genesis);
    assert_eq!(submit(&mut chain, next).unwrap_err(), BlockError::InsufficientWork);
}

#[test]
fn randomx_orphans_are_checked_once_their_parent_arrives() {
    let mut chain = rx_chain();
    let mut miner = Miner::default();
    miner.extend_by(&mut chain, 2, 1);
    let genesis = chain.genesis_hash();
    let b3 = miner.mine(&chain, chain.tip_hash(), 2);

    // Deux enfants du bloc 3, reçus avant lui : leur preuve ne peut pas encore
    // être vérifiée (la clé dépend de la branche), ils attendent leur parent.
    let mut with_b3 = rx_chain();
    let mut replay = Miner::default();
    replay.extend_by(&mut with_b3, 2, 1);
    extend(&mut with_b3, b3.clone());
    let good = miner.mine(&with_b3, b3.hash(), 3);
    let forged = miner.sha_only(&with_b3, b3.hash(), 4, genesis);
    for orphan in [forged.clone(), good.clone()] {
        assert!(matches!(submit(&mut chain, orphan).unwrap(), ProcessOutcome::Orphan { .. }));
    }

    // Le parent arrive : l'enfant valide est rattaché, l'autre écarté.
    submit(&mut chain, b3).unwrap();
    assert_eq!(chain.tip_hash(), good.hash());
    assert!(chain.entry(&forged.hash()).is_none());
    chain.check_consistency().unwrap();
}

#[test]
fn a_network_can_start_with_randomx_from_genesis() {
    // Comme le futur réseau principal : RandomX dès la genèse, dont la clé est
    // l'empreinte nulle (constat V-8 : ce chemin n'était exécuté par aucun test).
    fn build(nonce: u64, hash: Option<Hash256>) -> ConsensusParams {
        ConsensusParams {
            randomx: Some(RandomXParams {
                activation_height: 0,
                pow_limit: U256::low_bits(255),
                initial_target: U256::low_bits(254),
                key_interval: 4,
                key_lag: 1,
            }),
            genesis: GenesisParams {
                timestamp: 1_000_000,
                target: U256::low_bits(254),
                nonce,
                message: "randomx",
                hash,
            },
            ..regtest().clone()
        }
    }
    let draft: &'static ConsensusParams = Box::leak(Box::new(build(0, None)));
    let mut genesis = genesis_header(draft, 0);
    let hasher = RandomXHasher::new(&GENESIS_RANDOMX_KEY).unwrap();
    mine_header_randomx(&hasher, &mut genesis, 10_000).unwrap().unwrap();
    let params: &'static ConsensusParams = Box::leak(Box::new(build(genesis.nonce, Some(genesis.hash()))));
    assert_eq!(params.pow_algorithm(0), PowAlgorithm::RandomX);
    assert!(randomx_meets_target(&genesis, &GENESIS_RANDOMX_KEY).unwrap());

    let mut chain = Chain::new(params, MemoryStore::default()).unwrap();
    // Le bloc 1 prend pour clé l'identifiant de la genèse, et non l'empreinte nulle.
    let g = chain.genesis_hash();
    assert_eq!(chain.randomx_key_for_child(&g), Some(g));
    let mut miner = Miner::default();
    miner.extend_by(&mut chain, 6, 1);
    assert_eq!(chain.height(), 6);
    // Tout le travail est du travail RandomX.
    assert_eq!(chain.tip().chain_work.sha256d, U256::ZERO);
    chain.check_consistency().unwrap();
}

/// Bloc au parent inconnu (orphelin) qui annonce la hauteur `height`.
fn orphan_claiming(chain: &Chain<MemoryStore>, height: u64, tag: u8) -> Block {
    let mut block = unmined_block(chain, chain.tip_hash(), tag);
    block.header.prev_hash = Hash256([0xab; 32]);
    block.transactions[0] = Transaction::coinbase(height, vec![tag], block.transactions[0].outputs.clone());
    block.header.merkle_root = block.compute_merkle_root();
    block
}

#[test]
fn randomx_orphans_far_ahead_are_not_kept() {
    // Constat de la relecture de la 0.4.0 : un orphelin RandomX entrait dans
    // la réserve sans aucune preuve, et un bloc d'avant l'activation pouvait
    // s'y glisser en annonçant une hauteur RandomX.
    let mut chain = rx_chain();
    let mut miner = Miner::default();
    miner.extend_by(&mut chain, 2, 1);
    let is_orphan = |outcome: ProcessOutcome| matches!(outcome, ProcessOutcome::Orphan { .. });

    // Hauteur 20 : sa clé (bloc 16) n'est pas sur notre chaîne. Il n'est pas
    // gardé : renvoyé, il est encore un orphelin, jamais « déjà connu ».
    let far = orphan_claiming(&chain, 20, 1);
    assert!(is_orphan(submit(&mut chain, far.clone()).unwrap()));
    assert!(is_orphan(submit(&mut chain, far).unwrap()));

    // Hauteur 4 : clé connue (la genèse), il attend son parent.
    let near = orphan_claiming(&chain, 4, 2);
    assert!(is_orphan(submit(&mut chain, near.clone()).unwrap()));
    assert!(matches!(submit(&mut chain, near).unwrap(), ProcessOutcome::AlreadyKnown));

    // Un orphelin en SHA-256 double (preuve vérifiée) est gardé comme avant.
    let mut sha = orphan_claiming(&chain, ACTIVATION - 1, 3);
    sha = with_nonce(sha, BlockHeader::meets_target);
    assert!(is_orphan(submit(&mut chain, sha.clone()).unwrap()));
    assert!(matches!(submit(&mut chain, sha).unwrap(), ProcessOutcome::AlreadyKnown));
}

#[test]
fn structure_check_uses_the_claimed_height() {
    // Limite RandomX plus stricte que celle de SHA-256 (2^255).
    let params = rx_params_with(U256::low_bits(250));
    let chain = Chain::new(params, MemoryStore::default()).unwrap();
    let base = unmined_block(&chain, chain.genesis_hash(), 1);
    let claiming = |height: u64, target: U256| {
        let mut block = base.clone();
        block.transactions[0] = Transaction::coinbase(height, vec![1], block.transactions[0].outputs.clone());
        block.header.merkle_root = block.compute_merkle_root();
        block.header.target = target;
        block
    };
    // Hauteur annoncée avant l'activation : limite et preuve SHA-256 double.
    let sha_era = with_nonce(claiming(1, U256::low_bits(252)), BlockHeader::meets_target);
    check_block_structure(&sha_era, params).unwrap();
    let sha_fail = with_nonce(claiming(1, U256::low_bits(250)), |h| !h.meets_target());
    assert_eq!(check_block_structure(&sha_fail, params), Err(BlockError::InsufficientWork));
    // Après l'activation : limite RandomX, et pas de preuve SHA-256 exigée
    // (la preuve RandomX est vérifiée dans le contexte de la chaîne).
    let too_easy = claiming(ACTIVATION, U256::low_bits(252));
    assert_eq!(check_block_structure(&too_easy, params), Err(BlockError::TargetOutOfRange));
    let rx_era = with_nonce(claiming(ACTIVATION, U256::low_bits(250)), |h| !h.meets_target());
    check_block_structure(&rx_era, params).unwrap();
}
