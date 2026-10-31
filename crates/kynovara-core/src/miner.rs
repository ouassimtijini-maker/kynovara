//! Construction des blocs à miner et boucle de recherche du nonce.
//!
//! Optimisation classique : les 64 premiers octets de l'en-tête ne changent
//! pas pendant la recherche, l'état interne de SHA-256 après ces 64 octets
//! (« midstate ») est donc calculé une seule fois. Chaque essai ne coûte
//! ensuite que deux compressions SHA-256.

use crate::block::{BLOCK_VERSION, Block, BlockHeader, HEADER_SIZE, MAX_BLOCK_SIZE};
use crate::chain::{BlockStore, Chain};
use crate::crypto::PubKeyHash;
use crate::hash::Hash256;
use crate::mempool::Mempool;
use crate::merkle::merkle_root;
use crate::tx::{MAX_COINBASE_EXTRA, Transaction, TxOut};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Place réservée à l'en-tête et à la récompense dans un bloc.
const COINBASE_RESERVE: usize = 1_000;

/// Modèle de bloc prêt à miner (nonce à trouver).
#[derive(Clone, Debug)]
pub struct BlockTemplate {
    pub block: Block,
    pub height: u64,
    pub fees: u64,
}

/// Construit un modèle de bloc sur le sommet actuel de la chaîne.
pub fn create_block_template<S: BlockStore>(
    chain: &Chain<S>,
    mempool: &Mempool,
    recipient: PubKeyHash,
    now: u64,
    extra: &[u8],
) -> BlockTemplate {
    let tip = chain.tip();
    let height = tip.height + 1;
    let selected = if chain.params().transfers_allowed(height) {
        mempool.select_for_block(MAX_BLOCK_SIZE - HEADER_SIZE - COINBASE_RESERVE)
    } else {
        Vec::new()
    };
    let fees: u64 = selected.iter().map(|e| e.fee).fold(0u64, u64::saturating_add);
    let reward = chain.params().block_subsidy(height).saturating_add(fees);
    let mut extra = extra.to_vec();
    extra.truncate(MAX_COINBASE_EXTRA);
    // Récompense nulle (fin de l'émission, aucun frais) : pas de sortie du
    // tout, car une sortie de montant nul serait refusée.
    let outputs = if reward > 0 { vec![TxOut { value: reward, recipient }] } else { Vec::new() };
    let coinbase = Transaction::coinbase(height, extra, outputs);
    let mut transactions = Vec::with_capacity(selected.len() + 1);
    transactions.push(coinbase);
    transactions.extend(selected.into_iter().map(|e| e.tx.clone()));
    let ids: Vec<Hash256> = transactions.iter().map(Transaction::wtxid).collect();
    let target = chain.next_target_after(&tip.hash).unwrap_or(chain.params().pow_limit);
    let header = BlockHeader {
        version: BLOCK_VERSION,
        prev_hash: tip.hash,
        merkle_root: merkle_root(&ids),
        timestamp: now.max(tip.header.timestamp + 1),
        target,
        nonce: 0,
    };
    BlockTemplate { block: Block { header, transactions }, height, fees }
}

/// Résultat d'une session de minage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MineResult {
    /// Nonce gagnant trouvé.
    Found(u64),
    /// Nombre d'essais épuisé.
    Exhausted,
    /// Arrêt demandé.
    Stopped,
}

/// Cherche un nonce `start, start + step, start + 2·step…` pendant au plus
/// `max_attempts` essais. `hashes` est incrémenté du nombre d'essais (pour
/// mesurer la puissance de calcul) ; `stop` interrompt la recherche.
pub fn mine(
    header: &BlockHeader,
    start: u64,
    step: u64,
    max_attempts: u64,
    stop: &AtomicBool,
    hashes: &AtomicU64,
) -> MineResult {
    let bytes = header.to_array();
    let target = header.target.to_be_bytes();
    let mut midstate = Sha256::new();
    midstate.update(&bytes[..64]);
    let mut tail = [0u8; HEADER_SIZE - 64];
    tail.copy_from_slice(&bytes[64..]);
    let nonce_offset = tail.len() - 8;

    let step = step.max(1);
    let mut nonce = start;
    let mut done: u64 = 0;
    let mut reported: u64 = 0;
    if stop.load(Ordering::Relaxed) {
        return MineResult::Stopped;
    }
    while done < max_attempts {
        if done - reported >= 4096 {
            hashes.fetch_add(done - reported, Ordering::Relaxed);
            reported = done;
            if stop.load(Ordering::Relaxed) {
                return MineResult::Stopped;
            }
        }
        tail[nonce_offset..].copy_from_slice(&nonce.to_le_bytes());
        let mut h = midstate.clone();
        h.update(tail);
        let first: [u8; 32] = h.finalize().into();
        let second: [u8; 32] = Sha256::digest(first).into();
        done += 1;
        if second <= target {
            hashes.fetch_add(done - reported, Ordering::Relaxed);
            return MineResult::Found(nonce);
        }
        nonce = nonce.wrapping_add(step);
    }
    hashes.fetch_add(done - reported, Ordering::Relaxed);
    MineResult::Exhausted
}

/// Mine un en-tête dans le fil d'exécution courant (regtest, outils).
/// Renvoie `None` si aucun nonce n'a été trouvé en `max_attempts` essais.
pub fn mine_header(header: &mut BlockHeader, max_attempts: u64) -> Option<u64> {
    let stop = AtomicBool::new(false);
    let hashes = AtomicU64::new(0);
    match mine(header, 0, 1, max_attempts, &stop, &hashes) {
        MineResult::Found(nonce) => {
            header.nonce = nonce;
            Some(nonce)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::u256::U256;

    #[test]
    fn midstate_matches_full_hash() {
        let mut header = BlockHeader {
            version: 1,
            prev_hash: Hash256([1; 32]),
            merkle_root: Hash256([2; 32]),
            timestamp: 1_800_000_000,
            target: U256::low_bits(248),
            nonce: 0,
        };
        let nonce = mine_header(&mut header, 1_000_000).unwrap();
        assert_eq!(header.nonce, nonce);
        assert!(header.meets_target());
        // Le nonce trouvé est le premier qui convient.
        for n in 0..nonce {
            let mut h = header;
            h.nonce = n;
            assert!(!h.meets_target());
        }
    }

    #[test]
    fn stop_flag_and_exhaustion() {
        let header = BlockHeader {
            version: 1,
            prev_hash: Hash256::ZERO,
            merkle_root: Hash256::ZERO,
            timestamp: 0,
            target: U256::ZERO,
            nonce: 0,
        };
        let stop = AtomicBool::new(false);
        let hashes = AtomicU64::new(0);
        assert_eq!(mine(&header, 0, 1, 10_000, &stop, &hashes), MineResult::Exhausted);
        assert_eq!(hashes.load(Ordering::Relaxed), 10_000);
        stop.store(true, Ordering::Relaxed);
        assert_eq!(mine(&header, 0, 1, 10_000, &stop, &hashes), MineResult::Stopped);
    }
}
