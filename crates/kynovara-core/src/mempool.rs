//! Mempool : transactions valides en attente d'inclusion dans un bloc.
//!
//! Politique (propre à chaque nœud, ce ne sont pas des règles de consensus) :
//! - seules les pièces **confirmées** peuvent être dépensées (pas de chaîne de
//!   transactions non confirmées) ;
//! - pas de remplacement : la première transaction qui dépense une pièce
//!   gagne ;
//! - frais minimaux par octet ;
//! - taille totale bornée : les transactions aux frais par octet les plus
//!   faibles sont évincées en premier.

use crate::block::Block;
use crate::encode::Encode;
use crate::hash::Hash256;
use crate::params::ConsensusParams;
use crate::tx::{OutPoint, Transaction, TxOut};
use crate::utxo::{UtxoEntry, UtxoSet};
use crate::validation::{TxError, check_tx_inputs_unsigned, check_tx_structure, verify_input_signatures};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt;

/// Paramètres de politique de la mempool.
#[derive(Clone, Copy, Debug)]
pub struct MempoolPolicy {
    /// Frais minimaux, en unités par octet.
    pub min_fee_rate: u64,
    /// Taille totale maximale (octets).
    pub max_bytes: usize,
}

impl Default for MempoolPolicy {
    fn default() -> Self {
        MempoolPolicy { min_fee_rate: 1, max_bytes: 64 * 1024 * 1024 }
    }
}

/// Raison du refus d'une transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MempoolError {
    /// Règle de consensus violée.
    Invalid(TxError),
    AlreadyKnown,
    /// Une autre transaction de la mempool dépense déjà cette pièce.
    Conflict(OutPoint),
    FeeTooLow {
        fee: u64,
        required: u64,
    },
    MempoolFull,
    /// Alerte de points de contrôle en cours : transactions suspendues.
    Suspended,
    /// Période de rodage : aucun envoi avant ce bloc.
    TransfersLocked {
        unlock_height: u64,
    },
}

impl MempoolError {
    /// La transaction est-elle invalide quel que soit l'état de la chaîne
    /// (signature fausse, format…) ? Sert à pénaliser un pair.
    pub fn is_invalid_everywhere(&self) -> bool {
        match self {
            MempoolError::Invalid(e) => matches!(
                e,
                TxError::UnsupportedVersion(_)
                    | TxError::TooLarge(_)
                    | TxError::NoInputs
                    | TxError::NoOutputs
                    | TxError::ZeroValueOutput(_)
                    | TxError::OutputTooLarge(_)
                    | TxError::OutputSumTooLarge
                    | TxError::DuplicateInput(_)
                    | TxError::UnexpectedCoinbase
                    | TxError::WrongPublicKey(_)
                    | TxError::InvalidSignature(_)
            ),
            _ => false,
        }
    }
}

impl fmt::Display for MempoolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MempoolError::Invalid(e) => write!(f, "transaction invalide : {e}"),
            MempoolError::AlreadyKnown => write!(f, "transaction déjà dans la mempool"),
            MempoolError::Conflict(op) => {
                write!(f, "la pièce {op} est déjà dépensée par une transaction en attente")
            }
            MempoolError::FeeTooLow { fee, required } => {
                write!(f, "frais trop faibles : {fee} unités payées, {required} requises")
            }
            MempoolError::MempoolFull => write!(f, "mempool pleine et frais insuffisants pour y entrer"),
            MempoolError::TransfersLocked { unlock_height } => {
                write!(f, "période de rodage du réseau : envois possibles à partir du bloc {unlock_height}")
            }
            MempoolError::Suspended => {
                write!(f, "réseau en alerte (points de contrôle contradictoires) : transactions suspendues")
            }
        }
    }
}

impl std::error::Error for MempoolError {}

/// Résultat de [`Mempool::precheck`] : transaction acceptable sous réserve de
/// ses signatures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prechecked {
    pub txid: Hash256,
    pub fee: u64,
    pub size: usize,
    /// Sorties dépensées, dans l'ordre des entrées (pour l'empreinte de signature).
    pub spent: Vec<TxOut>,
    /// Transactions moins rémunératrices à évincer pour faire de la place.
    pub victims: Vec<Hash256>,
}

/// Transaction en attente.
#[derive(Clone, Debug)]
pub struct MempoolEntry {
    pub tx: Transaction,
    pub txid: Hash256,
    pub size: usize,
    pub fee: u64,
    /// Ordre d'arrivée.
    pub seq: u64,
}

/// Compare deux frais par octet, `fee_a / size_a` et `fee_b / size_b`, sans
/// division ni flottant.
fn rate_cmp(fee_a: u64, size_a: usize, fee_b: u64, size_b: usize) -> Ordering {
    (u128::from(fee_a) * size_b as u128).cmp(&(u128::from(fee_b) * size_a as u128))
}

impl MempoolEntry {
    /// Compare les frais par octet.
    fn fee_rate_cmp(&self, other: &MempoolEntry) -> Ordering {
        rate_cmp(self.fee, self.size, other.fee, other.size)
    }
}

/// La mempool.
#[derive(Debug, Default)]
pub struct Mempool {
    policy: MempoolPolicy,
    entries: HashMap<Hash256, MempoolEntry>,
    spends: HashMap<OutPoint, Hash256>,
    total_bytes: usize,
    next_seq: u64,
}

impl Mempool {
    pub fn new(policy: MempoolPolicy) -> Self {
        Mempool { policy, ..Mempool::default() }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    pub fn contains(&self, txid: &Hash256) -> bool {
        self.entries.contains_key(txid)
    }

    pub fn get(&self, txid: &Hash256) -> Option<&MempoolEntry> {
        self.entries.get(txid)
    }

    /// Transaction de la mempool qui dépense `outpoint`, s'il y en a une.
    pub fn spender_of(&self, outpoint: &OutPoint) -> Option<Hash256> {
        self.spends.get(outpoint).copied()
    }

    pub fn entries(&self) -> impl Iterator<Item = &MempoolEntry> {
        self.entries.values()
    }

    /// Valide puis ajoute une transaction. `next_height` est la hauteur du
    /// prochain bloc.
    ///
    /// Ordre des vérifications : d'abord tout ce qui est bon marché (format,
    /// doublon, conflit, pièces, frais, place disponible), et seulement ensuite
    /// les signatures. Une transaction vouée au refus (frais nuls, mempool
    /// pleine…) ne coûte donc presque rien à vérifier, même avec des centaines
    /// d'entrées.
    pub fn accept(
        &mut self,
        tx: Transaction,
        utxo: &UtxoSet,
        next_height: u64,
        params: &ConsensusParams,
        genesis_hash: &Hash256,
    ) -> Result<Hash256, MempoolError> {
        let checked = self.precheck(&tx, utxo, next_height, params)?;
        verify_input_signatures(&tx, &checked.spent, genesis_hash).map_err(MempoolError::Invalid)?;
        Ok(self.insert_checked(tx, checked))
    }

    /// Toutes les vérifications sauf les signatures, sans rien modifier. Le
    /// nœud s'en sert pour vérifier les signatures **hors** de son verrou
    /// principal : `precheck` sous le verrou, signatures sans le verrou, puis
    /// `precheck` à nouveau et [`insert_checked`](Self::insert_checked) sous le
    /// verrou (l'état a pu changer entre-temps).
    pub fn precheck(
        &self,
        tx: &Transaction,
        utxo: &UtxoSet,
        next_height: u64,
        params: &ConsensusParams,
    ) -> Result<Prechecked, MempoolError> {
        check_tx_structure(tx).map_err(MempoolError::Invalid)?;
        if tx.is_coinbase() {
            return Err(MempoolError::Invalid(TxError::UnexpectedCoinbase));
        }
        if !params.transfers_allowed(next_height) {
            return Err(MempoolError::TransfersLocked { unlock_height: params.transfers_unlock_height });
        }
        let txid = tx.txid();
        if self.entries.contains_key(&txid) {
            return Err(MempoolError::AlreadyKnown);
        }
        for input in tx.inputs() {
            if self.spends.contains_key(&input.prev_out) {
                return Err(MempoolError::Conflict(input.prev_out));
            }
        }
        let lookup = |op: &OutPoint| -> Option<UtxoEntry> { utxo.get(op).copied() };
        let (fee, spent) = check_tx_inputs_unsigned(tx, lookup, next_height, params).map_err(MempoolError::Invalid)?;
        let size = tx.size();
        let required = self.policy.min_fee_rate.saturating_mul(size as u64);
        if fee < required {
            return Err(MempoolError::FeeTooLow { fee, required });
        }
        let victims = self.room_for(fee, size)?;
        Ok(Prechecked { txid, fee, size, spent, victims })
    }

    /// Ajoute une transaction vérifiée par [`precheck`](Self::precheck) dont
    /// les signatures ont été vérifiées, sans changement d'état entre les
    /// deux (l'appelant en est responsable).
    pub fn insert_checked(&mut self, tx: Transaction, checked: Prechecked) -> Hash256 {
        for victim in &checked.victims {
            self.remove(victim);
        }
        let Prechecked { txid, fee, size, .. } = checked;
        let entry = MempoolEntry { tx, txid, size, fee, seq: self.next_seq };
        self.next_seq += 1;
        for input in entry.tx.inputs() {
            self.spends.insert(input.prev_out, txid);
        }
        self.total_bytes += size;
        self.entries.insert(txid, entry);
        txid
    }

    /// Transactions à évincer pour faire place à une nouvelle (vide si la
    /// place suffit). Seules des transactions moins rémunératrices (frais par
    /// octet strictement inférieurs) peuvent être évincées.
    fn room_for(&self, fee: u64, size: usize) -> Result<Vec<Hash256>, MempoolError> {
        if self.total_bytes + size <= self.policy.max_bytes {
            return Ok(Vec::new());
        }
        let mut by_rate: Vec<&MempoolEntry> = self.entries.values().collect();
        by_rate.sort_by(|a, b| a.fee_rate_cmp(b).then(b.seq.cmp(&a.seq)));
        let mut freed = 0usize;
        let mut victims = Vec::new();
        for e in by_rate {
            if self.total_bytes - freed + size <= self.policy.max_bytes {
                break;
            }
            if rate_cmp(e.fee, e.size, fee, size) != Ordering::Less {
                return Err(MempoolError::MempoolFull);
            }
            freed += e.size;
            victims.push(e.txid);
        }
        if self.total_bytes - freed + size > self.policy.max_bytes {
            return Err(MempoolError::MempoolFull);
        }
        Ok(victims)
    }

    /// Les `n` transactions aux frais par octet les plus élevés (annonce de
    /// la mempool à un nouveau pair).
    pub fn best_txids(&self, n: usize) -> Vec<Hash256> {
        let mut all: Vec<&MempoolEntry> = self.entries.values().collect();
        all.sort_by(|a, b| b.fee_rate_cmp(a).then(a.seq.cmp(&b.seq)));
        all.into_iter().take(n).map(|e| e.txid).collect()
    }

    /// Retire une transaction.
    pub fn remove(&mut self, txid: &Hash256) -> Option<MempoolEntry> {
        let entry = self.entries.remove(txid)?;
        for input in entry.tx.inputs() {
            self.spends.remove(&input.prev_out);
        }
        self.total_bytes -= entry.size;
        Some(entry)
    }

    /// Retire les transactions incluses dans un bloc et celles qui dépensent
    /// les mêmes pièces.
    pub fn remove_for_block(&mut self, block: &Block) {
        for tx in &block.transactions {
            self.remove(&tx.txid());
            for input in tx.inputs() {
                if let Some(conflict) = self.spends.get(&input.prev_out).copied() {
                    self.remove(&conflict);
                }
            }
        }
    }

    /// Retire les transactions devenues invalides (après une réorganisation).
    pub fn revalidate(&mut self, utxo: &UtxoSet, next_height: u64, params: &ConsensusParams) {
        // Retour dans la période de rodage (réorganisation) : plus aucun envoi possible.
        if !params.transfers_allowed(next_height) {
            let all: Vec<Hash256> = self.entries.keys().copied().collect();
            for txid in all {
                self.remove(&txid);
            }
            return;
        }
        let stale: Vec<Hash256> = self
            .entries
            .values()
            .filter(|e| {
                e.tx.inputs().iter().any(|i| match utxo.get(&i.prev_out) {
                    None => true,
                    Some(entry) => !entry.is_mature(next_height, params.coinbase_maturity),
                })
            })
            .map(|e| e.txid)
            .collect();
        for txid in stale {
            self.remove(&txid);
        }
    }

    /// Choisit les transactions à inclure dans un bloc (frais par octet
    /// décroissants) sans dépasser `max_bytes`.
    pub fn select_for_block(&self, max_bytes: usize) -> Vec<&MempoolEntry> {
        let mut all: Vec<&MempoolEntry> = self.entries.values().collect();
        all.sort_by(|a, b| b.fee_rate_cmp(a).then(a.seq.cmp(&b.seq)));
        let mut used = 0usize;
        let mut out = Vec::new();
        for e in all {
            if used + e.size <= max_bytes {
                used += e.size;
                out.push(e);
            }
        }
        out
    }

    /// Transactions encodées (pour le relais).
    pub fn raw(&self, txid: &Hash256) -> Option<Vec<u8>> {
        self.entries.get(txid).map(|e| e.tx.to_bytes())
    }
}
