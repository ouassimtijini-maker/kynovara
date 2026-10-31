//! Bloc de genèse : le premier bloc de chaque réseau, identique pour tous les
//! nœuds et inscrit en dur dans le code.
//!
//! Sa transaction de récompense paie une empreinte nulle dont personne ne
//! connaît de clé : ces pièces ne sont jamais ajoutées à l'ensemble UTXO
//! (aucune « pré-extraction »).

use crate::block::{BLOCK_VERSION, Block, BlockHeader};
use crate::crypto::PubKeyHash;
use crate::hash::Hash256;
use crate::merkle::merkle_root;
use crate::params::ConsensusParams;
use crate::tx::{Transaction, TxOut};

/// Destinataire de la récompense de genèse (inutilisable).
pub const GENESIS_RECIPIENT: PubKeyHash = PubKeyHash([0; 20]);

/// Transaction de récompense du bloc de genèse.
pub fn genesis_coinbase(params: &ConsensusParams) -> Transaction {
    Transaction::coinbase(
        0,
        params.genesis.message.as_bytes().to_vec(),
        vec![TxOut { value: params.initial_subsidy, recipient: GENESIS_RECIPIENT }],
    )
}

/// En-tête de genèse avec un nonce donné (sert aussi à miner la genèse).
pub fn genesis_header(params: &ConsensusParams, nonce: u64) -> BlockHeader {
    BlockHeader {
        version: BLOCK_VERSION,
        prev_hash: Hash256::ZERO,
        merkle_root: merkle_root(&[genesis_coinbase(params).wtxid()]),
        timestamp: params.genesis.timestamp,
        target: params.genesis.target,
        nonce,
    }
}

/// Bloc de genèse du réseau.
pub fn genesis_block(params: &ConsensusParams) -> Block {
    Block { header: genesis_header(params, params.genesis.nonce), transactions: vec![genesis_coinbase(params)] }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::Network;
    use crate::validation::check_block_structure;

    #[test]
    fn launched_genesis_blocks_are_valid() {
        for net in Network::ALL {
            let p = net.params();
            let Some(expected) = p.genesis.hash else { continue };
            let g = genesis_block(p);
            assert_eq!(g.header.hash(), expected, "{net}");
            check_block_structure(&g, p).unwrap();
        }
    }

    #[test]
    fn genesis_differs_per_network() {
        let a = genesis_block(Network::Testnet.params()).header.hash();
        let b = genesis_block(Network::Regtest.params()).header.hash();
        assert_ne!(a, b);
    }
}
