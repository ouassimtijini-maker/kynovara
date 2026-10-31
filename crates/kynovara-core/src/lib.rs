//! # kynovara-core
//!
//! Le cœur de la blockchain **Kynovara** : tout ce qui définit le consensus.
//!
//! Ce module ne fait aucune entrée/sortie réseau. Il contient :
//!
//! - les types de base (transactions, blocs, adresses, montants) et leur
//!   encodage binaire strict ([`encode`]) ;
//! - la cryptographie (hachage SHA-256, signatures Ed25519) ;
//! - la preuve de travail (SHA-256 double) et l'ajustement de difficulté
//!   (LWMA) ;
//! - la politique monétaire (récompense de bloc et divisions par deux) ;
//! - la validation complète des blocs et transactions ;
//! - l'état de la chaîne (ensemble UTXO, réorganisations) ;
//! - la mempool et la construction des blocs à miner.
//!
//! Tout ce qui est marqué « règle de consensus » doit être identique sur tous
//! les nœuds : modifier une telle règle après le lancement crée une
//! bifurcation (fork) du réseau.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod address;
pub mod amount;
pub mod bech32;
pub mod block;
pub mod chain;
pub mod checkpoint;
pub mod crypto;
pub mod difficulty;
pub mod encode;
pub mod genesis;
pub mod hash;
pub mod hex;
pub mod mempool;
pub mod merkle;
pub mod miner;
pub mod params;
pub mod time;
pub mod tx;
pub mod u256;
pub mod utxo;
pub mod validation;

pub use address::Address;
pub use block::{Block, BlockHeader};
pub use chain::Chain;
pub use crypto::{KeyPair, PubKeyHash, PublicKey, Signature};
pub use hash::Hash256;
pub use params::{ConsensusParams, Network};
pub use tx::{OutPoint, Transaction, TxIn, TxKind, TxOut};
pub use u256::U256;
