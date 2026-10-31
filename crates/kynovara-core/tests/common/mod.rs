//! Outils communs aux tests d'intégration.
#![allow(dead_code, clippy::unwrap_used)]

use kynovara_core::block::{BLOCK_VERSION, Block, BlockHeader};
use kynovara_core::chain::{BlockStore, Chain, MemoryStore, ProcessOutcome, TipChange};
use kynovara_core::crypto::{KeyPair, PubKeyHash};
use kynovara_core::hash::Hash256;
use kynovara_core::merkle::merkle_root;
use kynovara_core::miner::mine_header;
use kynovara_core::params::{ConsensusParams, Network};
use kynovara_core::tx::{OutPoint, Transaction, TxIn, TxOut};
use kynovara_core::validation::BlockError;

pub fn regtest() -> &'static ConsensusParams {
    Network::Regtest.params()
}

pub fn new_chain() -> Chain<MemoryStore> {
    Chain::new(regtest(), MemoryStore::default()).unwrap()
}

pub fn key(i: u8) -> KeyPair {
    KeyPair::from_secret_bytes(&[i; 32])
}

pub fn pkh(i: u8) -> PubKeyHash {
    key(i).public_key().pubkey_hash()
}

/// Construit et mine un bloc enfant de `parent` contenant `txs`, dont la
/// récompense (subvention + `fees`) va à `reward_to`.
pub fn make_block<S: BlockStore>(
    chain: &Chain<S>,
    parent: Hash256,
    txs: Vec<Transaction>,
    reward_to: PubKeyHash,
    fees: u64,
    tag: u8,
) -> Block {
    let parent_entry = chain.entry(&parent).expect("parent connu");
    let height = parent_entry.height + 1;
    let reward = chain.params().block_subsidy(height) + fees;
    // `tag` distingue des blocs frères qui seraient sinon identiques.
    let coinbase = Transaction::coinbase(height, vec![tag], vec![TxOut { value: reward, recipient: reward_to }]);
    let mut transactions = vec![coinbase];
    transactions.extend(txs);
    let ids: Vec<Hash256> = transactions.iter().map(Transaction::wtxid).collect();
    let mut header = BlockHeader {
        version: BLOCK_VERSION,
        prev_hash: parent,
        merkle_root: merkle_root(&ids),
        timestamp: parent_entry.header.timestamp + chain.params().target_spacing,
        target: chain.next_target_after(&parent).unwrap(),
        nonce: 0,
    };
    mine_header(&mut header, 10_000_000).expect("nonce trouvé");
    Block { header, transactions }
}

/// Signe un transfert dépensant `inputs` (tous verrouillés par `owner`).
pub fn spend(
    chain_genesis: &Hash256,
    owner: &KeyPair,
    inputs: &[(OutPoint, TxOut)],
    outputs: Vec<TxOut>,
) -> Transaction {
    let mut tx = Transaction::transfer(
        inputs
            .iter()
            .map(|(op, _)| TxIn {
                prev_out: *op,
                public_key: owner.public_key(),
                signature: kynovara_core::crypto::Signature([0; 64]),
            })
            .collect(),
        outputs,
    );
    let spent: Vec<TxOut> = inputs.iter().map(|(_, o)| *o).collect();
    let sighash = tx.sighash(chain_genesis, &spent);
    if let kynovara_core::tx::TxKind::Transfer { inputs } = &mut tx.kind {
        for input in inputs.iter_mut() {
            input.signature = owner.sign(&sighash.0);
        }
    }
    tx
}

/// Traite un bloc avec une horloge très en avance (pas de rejet « futur »).
pub fn submit<S: BlockStore>(chain: &mut Chain<S>, block: Block) -> Result<ProcessOutcome, BlockError> {
    chain.process_block(block, u64::MAX / 2)
}

/// Traite un bloc qui doit devenir le nouveau sommet.
pub fn extend<S: BlockStore>(chain: &mut Chain<S>, block: Block) -> TipChange {
    let hash = block.hash();
    match submit(chain, block).unwrap() {
        ProcessOutcome::Accepted { tip_change: Some(tc), invalid } => {
            assert!(invalid.is_empty(), "blocs invalides inattendus : {invalid:?}");
            assert_eq!(chain.tip_hash(), hash);
            tc
        }
        other => panic!("le bloc aurait dû étendre la chaîne : {other:?}"),
    }
}

/// Mine `n` blocs vides au sommet, récompenses pour `to`.
pub fn mine_empty<S: BlockStore>(chain: &mut Chain<S>, n: usize, to: PubKeyHash) -> Vec<Block> {
    let mut out = Vec::new();
    for _ in 0..n {
        let b = make_block(chain, chain.tip_hash(), vec![], to, 0, 0);
        out.push(b.clone());
        extend(chain, b);
    }
    out
}

/// Sortie de récompense du bloc actif à la hauteur `h`.
pub fn coinbase_output<S: BlockStore>(chain: &Chain<S>, h: u64) -> (OutPoint, TxOut) {
    let hash = chain.block_hash_at(h).unwrap();
    let block = chain.get_block(&hash).unwrap().unwrap();
    let cb = &block.transactions[0];
    (OutPoint { txid: cb.txid(), vout: 0 }, cb.outputs[0])
}
