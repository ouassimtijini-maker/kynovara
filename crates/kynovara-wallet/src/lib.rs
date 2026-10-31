//! # kynovara-wallet
//!
//! Portefeuille de Kynovara et outil en ligne de commande `kynovara-cli`.
//!
//! - phrase de récupération de 24 mots (BIP-39) ;
//! - dérivation hiérarchique des clés Ed25519 (SLIP-0010) ;
//! - fichier chiffré (Argon2id + XChaCha20-Poly1305) ;
//! - construction et signature des transactions ;
//! - pilotage du nœud par son interface RPC locale.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod bip39;
pub mod cli;
pub mod keystore;
pub mod prompt;
pub mod rpc_client;
pub mod slip10;
pub mod wallet;
