//! # kynovara-node
//!
//! Le nœud Kynovara (`kynovarad`) : il stocke la chaîne, discute avec les
//! autres nœuds, valide et relaie blocs et transactions, mine, et expose une
//! interface RPC locale pour le portefeuille.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod checkpoints;
pub mod config;
pub mod http;
pub mod logger;
mod mining;
pub mod node;
pub mod p2p;
mod rpc;
pub mod storage;

pub use config::{Command, NodeConfig, parse_args};
pub use node::{BlockResult, Node};
