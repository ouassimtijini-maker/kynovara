//! Ensemble des sorties non dépensées (UTXO).
//!
//! C'est « l'état » de la chaîne : la liste de toutes les pièces existantes et
//! de leur propriétaire. Un index secondaire par destinataire permet au
//! portefeuille de retrouver rapidement ses pièces.

use crate::crypto::PubKeyHash;
use crate::tx::{OutPoint, TxOut};
use std::collections::{BTreeSet, HashMap};

/// Une sortie non dépensée et son contexte.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct UtxoEntry {
    pub output: TxOut,
    /// Hauteur du bloc qui a créé la sortie.
    pub height: u64,
    /// Sortie d'une transaction de récompense (soumise à maturité).
    pub is_coinbase: bool,
}

impl UtxoEntry {
    /// Peut-elle être dépensée dans un bloc de hauteur `spend_height` ?
    pub fn is_mature(&self, spend_height: u64, maturity: u64) -> bool {
        !self.is_coinbase || spend_height >= self.height.saturating_add(maturity)
    }
}

/// Ensemble UTXO.
#[derive(Clone, Debug, Default)]
pub struct UtxoSet {
    entries: HashMap<OutPoint, UtxoEntry>,
    by_recipient: HashMap<PubKeyHash, BTreeSet<OutPoint>>,
    total_value: u128,
}

impl PartialEq for UtxoSet {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

impl Eq for UtxoSet {}

impl UtxoSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, outpoint: &OutPoint) -> Option<&UtxoEntry> {
        self.entries.get(outpoint)
    }

    pub fn contains(&self, outpoint: &OutPoint) -> bool {
        self.entries.contains_key(outpoint)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Somme de toutes les pièces existantes.
    pub fn total_value(&self) -> u128 {
        self.total_value
    }

    pub fn insert(&mut self, outpoint: OutPoint, entry: UtxoEntry) -> Option<UtxoEntry> {
        let previous = self.entries.insert(outpoint, entry);
        if let Some(old) = previous {
            self.unindex(&outpoint, &old);
        }
        self.by_recipient.entry(entry.output.recipient).or_default().insert(outpoint);
        self.total_value += u128::from(entry.output.value);
        previous
    }

    pub fn remove(&mut self, outpoint: &OutPoint) -> Option<UtxoEntry> {
        let removed = self.entries.remove(outpoint)?;
        self.unindex(outpoint, &removed);
        Some(removed)
    }

    fn unindex(&mut self, outpoint: &OutPoint, entry: &UtxoEntry) {
        if let Some(set) = self.by_recipient.get_mut(&entry.output.recipient) {
            set.remove(outpoint);
            if set.is_empty() {
                self.by_recipient.remove(&entry.output.recipient);
            }
        }
        self.total_value -= u128::from(entry.output.value);
    }

    /// Pièces d'un destinataire, triées par référence.
    pub fn for_recipient(&self, recipient: &PubKeyHash) -> Vec<(OutPoint, UtxoEntry)> {
        self.by_recipient
            .get(recipient)
            .map(|set| set.iter().filter_map(|op| self.entries.get(op).map(|e| (*op, *e))).collect())
            .unwrap_or_default()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&OutPoint, &UtxoEntry)> {
        self.entries.iter()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.by_recipient.clear();
        self.total_value = 0;
    }
}

/// Données nécessaires pour annuler un bloc : les sorties qu'il a dépensées.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockUndo {
    pub spent: Vec<(OutPoint, UtxoEntry)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::Hash256;

    fn entry(value: u64, who: u8) -> UtxoEntry {
        UtxoEntry { output: TxOut { value, recipient: PubKeyHash([who; 20]) }, height: 1, is_coinbase: false }
    }

    fn op(i: u8) -> OutPoint {
        OutPoint { txid: Hash256([i; 32]), vout: 0 }
    }

    #[test]
    fn index_and_totals() {
        let mut set = UtxoSet::new();
        set.insert(op(1), entry(10, 1));
        set.insert(op(2), entry(20, 1));
        set.insert(op(3), entry(30, 2));
        assert_eq!(set.total_value(), 60);
        assert_eq!(set.for_recipient(&PubKeyHash([1; 20])).len(), 2);
        assert_eq!(set.remove(&op(1)).unwrap().output.value, 10);
        assert_eq!(set.remove(&op(1)), None);
        assert_eq!(set.total_value(), 50);
        assert_eq!(set.for_recipient(&PubKeyHash([1; 20])).len(), 1);
        // Remplacement : l'index suit.
        set.insert(op(3), entry(5, 3));
        assert!(set.for_recipient(&PubKeyHash([2; 20])).is_empty());
        assert_eq!(set.total_value(), 25);
    }

    #[test]
    fn maturity() {
        let mut e = entry(1, 1);
        e.is_coinbase = true;
        e.height = 10;
        assert!(!e.is_mature(109, 100));
        assert!(e.is_mature(110, 100));
        e.is_coinbase = false;
        assert!(e.is_mature(10, 100));
    }
}
