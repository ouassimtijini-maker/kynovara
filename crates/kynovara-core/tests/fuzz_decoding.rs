//! Tests « fuzz » déterministes : des centaines de milliers d'entrées
//! aléatoires ou altérées ne doivent jamais faire paniquer le décodage, et tout
//! ce qui se décode doit se ré-encoder à l'identique (encodage canonique).
#![allow(clippy::unwrap_used)]

use kynovara_core::block::{Block, BlockHeader};
use kynovara_core::crypto::{KeyPair, PubKeyHash, Signature};
use kynovara_core::encode::{Decode, Encode};
use kynovara_core::genesis::genesis_block;
use kynovara_core::hash::Hash256;
use kynovara_core::params::Network;
use kynovara_core::tx::{OutPoint, Transaction, TxIn, TxOut};
use kynovara_core::validation::{check_block_structure, check_tx_structure};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn sample_block() -> Block {
    let mut b = genesis_block(Network::Regtest.params());
    let kp = KeyPair::from_secret_bytes(&[3; 32]);
    b.transactions.push(Transaction::transfer(
        vec![TxIn {
            prev_out: OutPoint { txid: Hash256([1; 32]), vout: 2 },
            public_key: kp.public_key(),
            signature: Signature([4; 64]),
        }],
        vec![TxOut { value: 5, recipient: PubKeyHash([6; 20]) }, TxOut { value: 7, recipient: PubKeyHash([8; 20]) }],
    ));
    b.header.merkle_root = b.compute_merkle_root();
    b
}

fn check_canonical(bytes: &[u8]) {
    if let Ok(block) = Block::from_bytes(bytes) {
        assert_eq!(block.to_bytes(), bytes, "encodage non canonique");
        let _ = check_block_structure(&block, Network::Regtest.params());
    }
    if let Ok(tx) = Transaction::from_bytes(bytes) {
        assert_eq!(tx.to_bytes(), bytes, "encodage non canonique");
        let _ = check_tx_structure(&tx);
        let _ = tx.txid();
    }
    if let Ok(h) = BlockHeader::from_bytes(bytes) {
        assert_eq!(h.to_bytes(), bytes);
    }
}

#[test]
fn random_bytes_never_panic() {
    let mut rng = Rng(1);
    for _ in 0..200_000 {
        let len = rng.below(400);
        let bytes: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        check_canonical(&bytes);
    }
}

#[test]
fn mutated_valid_encodings_never_panic() {
    let mut rng = Rng(2);
    let original = sample_block().to_bytes();
    for _ in 0..100_000 {
        let mut bytes = original.clone();
        match rng.below(4) {
            // Octets modifiés.
            0 => {
                for _ in 0..1 + rng.below(4) {
                    let i = rng.below(bytes.len());
                    bytes[i] = rng.next() as u8;
                }
            }
            // Troncature.
            1 => bytes.truncate(rng.below(bytes.len())),
            // Octets ajoutés.
            2 => {
                for _ in 0..1 + rng.below(8) {
                    bytes.push(rng.next() as u8);
                }
            }
            // Longueurs gonflées (champs de 4 octets).
            _ => {
                let i = rng.below(bytes.len().saturating_sub(4));
                bytes[i..i + 4].copy_from_slice(&(rng.next() as u32).to_le_bytes());
            }
        }
        check_canonical(&bytes);
    }
}

#[test]
fn transaction_mutations_change_identifiers() {
    let block = sample_block();
    let tx = &block.transactions[1];
    let bytes = tx.to_bytes();
    let mut rng = Rng(3);
    for _ in 0..20_000 {
        let mut m = bytes.clone();
        let i = rng.below(m.len());
        let delta = 1 + rng.below(255) as u8;
        m[i] = m[i].wrapping_add(delta);
        if let Ok(other) = Transaction::from_bytes(&m) {
            // Toute modification change le wtxid (engagé par l'arbre de Merkle).
            assert_ne!(other.wtxid(), tx.wtxid());
        }
    }
}
