//! Logique du portefeuille : dérivation des adresses, sélection des pièces,
//! construction et signature des transactions.

use crate::bip39;
use crate::slip10;
use kynovara_core::address::Address;
use kynovara_core::amount::{COIN, format_amount};
use kynovara_core::crypto::{KeyPair, PubKeyHash, Signature};
use kynovara_core::hash::Hash256;
use kynovara_core::params::Network;
use kynovara_core::tx::{INPUT_SIZE, MAX_TX_SIZE, OUTPUT_SIZE, OutPoint, Transaction, TxIn, TxKind, TxOut};
use std::collections::HashMap;
use std::fmt;
use zeroize::Zeroizing;

/// Sous ce montant (unités), on ne crée pas de monnaie rendue : la différence
/// part en frais (une pièce minuscule coûterait plus cher à dépenser qu'elle
/// ne vaut).
pub const DUST_THRESHOLD: u64 = 10_000;
/// Frais par octet par défaut (le minimum accepté par les nœuds est 1).
pub const DEFAULT_FEE_RATE: u64 = 2;
/// Nombre maximal d'entrées dans une transaction construite par le portefeuille.
pub const MAX_INPUTS: usize = (MAX_TX_SIZE - 1_000) / INPUT_SIZE;
/// Plafond de frais : au-delà, il s'agit presque sûrement d'une erreur de
/// saisie (`--fee-rate` trop élevé) et la transaction n'est pas construite.
/// Même la plus grande transaction possible au tarif par défaut paie 500 fois
/// moins.
pub const MAX_FEE: u64 = COIN;

/// Branche de dérivation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Branch {
    Receive,
    Change,
}

impl Branch {
    fn index(self) -> u32 {
        match self {
            Branch::Receive => 0,
            Branch::Change => 1,
        }
    }
}

/// Pièce disponible, telle que rapportée par le nœud.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalletUtxo {
    pub outpoint: OutPoint,
    pub value: u64,
    pub recipient: PubKeyHash,
    pub mature: bool,
    pub spent_in_mempool: bool,
}

/// Erreur de construction de transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildError {
    InsufficientFunds { available: u64, needed: u64 },
    TooManyInputs,
    AmountTooSmall,
    UnknownKey(PubKeyHash),
    WrongNetwork,
    FeeTooHigh { fee: u64 },
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildError::InsufficientFunds { available, needed } => write!(
                f,
                "fonds insuffisants : {} KYNO disponibles, {} KYNO nécessaires (frais compris)",
                format_amount(*available),
                format_amount(*needed)
            ),
            BuildError::TooManyInputs => write!(
                f,
                "trop de petites pièces seraient nécessaires ; envoie d'abord un montant plus petit pour les regrouper"
            ),
            BuildError::AmountTooSmall => write!(f, "montant trop petit (moins que le seuil de poussière)"),
            BuildError::UnknownKey(pkh) => write!(f, "clé introuvable pour l'empreinte {pkh}"),
            BuildError::WrongNetwork => write!(f, "l'adresse de destination appartient à un autre réseau"),
            BuildError::FeeTooHigh { fee } => write!(
                f,
                "frais anormalement élevés ({} KYNO, plafond {} KYNO) : vérifie --fee-rate",
                format_amount(*fee),
                format_amount(MAX_FEE)
            ),
        }
    }
}

impl std::error::Error for BuildError {}

/// Transaction prête à diffuser.
#[derive(Debug, Clone)]
pub struct PreparedTx {
    pub tx: Transaction,
    pub amount: u64,
    pub fee: u64,
    pub change: Option<(Address, u64)>,
    pub inputs_total: u64,
}

/// Portefeuille déverrouillé (secrets effacés de la mémoire à la fin).
pub struct Wallet {
    network: Network,
    seed: Zeroizing<[u8; 64]>,
    pub receive_index: u32,
    pub change_index: u32,
}

impl fmt::Debug for Wallet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Wallet")
            .field("network", &self.network)
            .field("receive_index", &self.receive_index)
            .field("change_index", &self.change_index)
            .finish_non_exhaustive()
    }
}

/// Taille d'un transfert à `inputs` entrées et `outputs` sorties.
pub fn transfer_size(inputs: usize, outputs: usize) -> usize {
    4 + 1 + 4 + inputs * INPUT_SIZE + 4 + outputs * OUTPUT_SIZE
}

impl Wallet {
    /// Portefeuille à partir de l'entropie de la phrase de 24 mots.
    pub fn from_entropy(network: Network, entropy: &[u8; 32], receive_index: u32, change_index: u32) -> Self {
        let words = bip39::entropy_to_words(entropy);
        let seed = bip39::mnemonic_to_seed_no_passphrase(&words);
        Wallet { network, seed, receive_index, change_index }
    }

    pub fn network(&self) -> Network {
        self.network
    }

    /// Clé de l'adresse `index` d'une branche.
    pub fn key(&self, branch: Branch, index: u32) -> KeyPair {
        let coin = self.network.params().slip44_coin_type;
        let k = slip10::derive_path(&self.seed[..], &[44, coin, 0, branch.index(), index]);
        KeyPair::from_secret_bytes(&k.key)
    }

    pub fn address(&self, branch: Branch, index: u32) -> Address {
        Address::from_public_key(self.network, &self.key(branch, index).public_key())
    }

    /// Distribue une nouvelle adresse de réception.
    pub fn next_receive_address(&mut self) -> Address {
        let a = self.address(Branch::Receive, self.receive_index);
        self.receive_index += 1;
        a
    }

    /// Toutes les adresses déjà distribuées (au moins la première de réception).
    pub fn known_addresses(&self) -> Vec<(Branch, u32, Address)> {
        let mut out = Vec::new();
        for i in 0..self.receive_index.max(1) {
            out.push((Branch::Receive, i, self.address(Branch::Receive, i)));
        }
        for i in 0..self.change_index {
            out.push((Branch::Change, i, self.address(Branch::Change, i)));
        }
        out
    }

    /// Prépare (et signe) un envoi de `amount` unités à `to`. Avec `amount =
    /// None`, envoie tout le solde disponible (moins les frais).
    pub fn build_transaction(
        &mut self,
        utxos: &[WalletUtxo],
        to: &Address,
        amount: Option<u64>,
        fee_rate: u64,
        genesis_hash: &Hash256,
    ) -> Result<PreparedTx, BuildError> {
        if to.network() != self.network {
            return Err(BuildError::WrongNetwork);
        }
        let mut candidates: Vec<&WalletUtxo> =
            utxos.iter().filter(|u| u.mature && !u.spent_in_mempool && u.value > 0).collect();
        // Les plus grosses pièces d'abord (moins d'entrées, moins de frais).
        candidates.sort_by(|a, b| b.value.cmp(&a.value).then(a.outpoint.cmp(&b.outpoint)));
        let available: u64 = candidates.iter().map(|u| u.value).fold(0, u64::saturating_add);

        let (selected, amount, fee, change_value) = match amount {
            None => {
                let selected: Vec<&WalletUtxo> = candidates.iter().copied().take(MAX_INPUTS).collect();
                if selected.len() < candidates.len() {
                    return Err(BuildError::TooManyInputs);
                }
                // Sommes saturées : les montants viennent du nœud, qui pourrait
                // mentir (sans risque pour les fonds : les signatures engagent
                // les vrais montants, une transaction faussée serait refusée).
                let total: u64 = selected.iter().map(|u| u.value).fold(0, u64::saturating_add);
                let fee = fee_rate.saturating_mul(transfer_size(selected.len(), 1) as u64);
                if total <= fee + DUST_THRESHOLD {
                    return Err(BuildError::InsufficientFunds { available, needed: fee + DUST_THRESHOLD + 1 });
                }
                (selected, total - fee, fee, 0)
            }
            Some(amount) => {
                if amount < DUST_THRESHOLD {
                    return Err(BuildError::AmountTooSmall);
                }
                let mut selected = Vec::new();
                let mut total: u64 = 0;
                let mut result = None;
                for u in &candidates {
                    if selected.len() >= MAX_INPUTS {
                        return Err(BuildError::TooManyInputs);
                    }
                    selected.push(*u);
                    total = total.saturating_add(u.value);
                    let fee_with_change = fee_rate.saturating_mul(transfer_size(selected.len(), 2) as u64);
                    if total >= amount.saturating_add(fee_with_change) {
                        let change = total - amount - fee_with_change;
                        result = Some(if change < DUST_THRESHOLD {
                            // Pas de monnaie rendue : le reliquat va aux frais.
                            (total - amount, 0)
                        } else {
                            (fee_with_change, change)
                        });
                        break;
                    }
                }
                let Some((fee, change)) = result else {
                    let needed = amount
                        .saturating_add(fee_rate.saturating_mul(transfer_size(candidates.len().max(1), 2) as u64));
                    return Err(BuildError::InsufficientFunds { available, needed });
                };
                (selected, amount, fee, change)
            }
        };
        if fee > MAX_FEE {
            return Err(BuildError::FeeTooHigh { fee });
        }

        // Clés de toutes les adresses connues, indexées par empreinte.
        let mut keys: HashMap<PubKeyHash, KeyPair> = HashMap::new();
        for (branch, index, address) in self.known_addresses() {
            keys.insert(address.pubkey_hash(), self.key(branch, index));
        }
        let mut outputs = vec![TxOut { value: amount, recipient: to.pubkey_hash() }];
        let change = if change_value > 0 {
            let change_address = self.address(Branch::Change, self.change_index);
            keys.insert(change_address.pubkey_hash(), self.key(Branch::Change, self.change_index));
            self.change_index += 1;
            outputs.push(TxOut { value: change_value, recipient: change_address.pubkey_hash() });
            Some((change_address, change_value))
        } else {
            None
        };

        let mut inputs = Vec::with_capacity(selected.len());
        let mut spent = Vec::with_capacity(selected.len());
        for u in &selected {
            let key = keys.get(&u.recipient).ok_or(BuildError::UnknownKey(u.recipient))?;
            inputs.push(TxIn { prev_out: u.outpoint, public_key: key.public_key(), signature: Signature([0; 64]) });
            spent.push(TxOut { value: u.value, recipient: u.recipient });
        }
        let mut tx = Transaction::transfer(inputs, outputs);
        let sighash = tx.sighash(genesis_hash, &spent);
        if let TxKind::Transfer { inputs } = &mut tx.kind {
            for input in inputs.iter_mut() {
                let key = keys
                    .get(&input.public_key.pubkey_hash())
                    .ok_or(BuildError::UnknownKey(input.public_key.pubkey_hash()))?;
                input.signature = key.sign(&sighash.0);
            }
        }
        let inputs_total = selected.iter().map(|u| u.value).fold(0, u64::saturating_add);
        Ok(PreparedTx { tx, amount, fee, change, inputs_total })
    }
}

/// Clé maîtresse des points de contrôle, dérivée de sa propre phrase de
/// 24 mots (chemin `m/44'/type'/9000'/0'/0'`, distinct des portefeuilles).
pub fn checkpoint_master_key(network: Network, entropy: &[u8; 32]) -> KeyPair {
    let words = bip39::entropy_to_words(entropy);
    let seed = bip39::mnemonic_to_seed_no_passphrase(&words);
    let coin = network.params().slip44_coin_type;
    let k = slip10::derive_path(&seed[..], &[44, coin, kynovara_core::checkpoint::MASTER_KEY_ACCOUNT, 0, 0]);
    KeyPair::from_secret_bytes(&k.key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regtest_master_key_comes_from_its_public_phrase() {
        // Phrase publique du regtest : « abandon » × 23 puis « art » (entropie nulle).
        let key = checkpoint_master_key(Network::Regtest, &[0u8; 32]);
        assert_eq!(Some(key.public_key()), Network::Regtest.params().checkpoint_master);
        let secret = KeyPair::from_secret_bytes(&kynovara_core::params::REGTEST_CHECKPOINT_SECRET);
        assert_eq!(secret.public_key(), key.public_key());
    }

    #[test]
    fn checkpoint_master_key_is_deterministic_and_separate_from_wallet_keys() {
        let e = [9u8; 32];
        let a = checkpoint_master_key(Network::Testnet, &e);
        assert_eq!(a.public_key(), checkpoint_master_key(Network::Testnet, &e).public_key());
        assert_ne!(a.public_key(), checkpoint_master_key(Network::Mainnet, &e).public_key());
        let w = Wallet::from_entropy(Network::Testnet, &e, 0, 0);
        for i in 0..5 {
            assert_ne!(a.public_key(), w.key(Branch::Receive, i).public_key());
            assert_ne!(a.public_key(), w.key(Branch::Change, i).public_key());
        }
    }
    use kynovara_core::amount::COIN;
    use kynovara_core::utxo::{UtxoEntry, UtxoSet};
    use kynovara_core::validation::check_tx_inputs;

    fn wallet() -> Wallet {
        let mut w = Wallet::from_entropy(Network::Regtest, &[9u8; 32], 0, 0);
        for _ in 0..3 {
            w.next_receive_address();
        }
        w
    }

    fn utxo(w: &Wallet, i: u32, value: u64, n: u8) -> WalletUtxo {
        WalletUtxo {
            outpoint: OutPoint { txid: Hash256([n; 32]), vout: 0 },
            value,
            recipient: w.address(Branch::Receive, i).pubkey_hash(),
            mature: true,
            spent_in_mempool: false,
        }
    }

    fn genesis() -> Hash256 {
        Network::Regtest.params().genesis.hash.unwrap()
    }

    /// La transaction construite passe la validation de consensus.
    fn assert_valid(tx: &Transaction, utxos: &[WalletUtxo]) -> u64 {
        let mut set = UtxoSet::new();
        for u in utxos {
            set.insert(
                u.outpoint,
                UtxoEntry { output: TxOut { value: u.value, recipient: u.recipient }, height: 1, is_coinbase: false },
            );
        }
        kynovara_core::validation::check_tx_structure(tx).unwrap();
        check_tx_inputs(tx, |op| set.get(op).copied(), 2, Network::Regtest.params(), &genesis()).unwrap()
    }

    #[test]
    fn derivation_is_deterministic_and_distinct() {
        let w = wallet();
        assert_eq!(w.address(Branch::Receive, 0), wallet().address(Branch::Receive, 0));
        assert_ne!(w.address(Branch::Receive, 0), w.address(Branch::Receive, 1));
        assert_ne!(w.address(Branch::Receive, 0), w.address(Branch::Change, 0));
        // Réseau différent : clés différentes (type de pièce SLIP-44) ou préfixe différent.
        let main = Wallet::from_entropy(Network::Mainnet, &[9u8; 32], 0, 0);
        assert_ne!(main.address(Branch::Receive, 0).pubkey_hash(), w.address(Branch::Receive, 0).pubkey_hash());
        assert_eq!(w.known_addresses().len(), 3);
    }

    #[test]
    fn send_with_change() {
        let mut w = wallet();
        let utxos = vec![utxo(&w, 0, 10 * COIN, 1), utxo(&w, 1, 3 * COIN, 2), utxo(&w, 2, COIN, 3)];
        let dest = Wallet::from_entropy(Network::Regtest, &[1u8; 32], 0, 0).address(Branch::Receive, 0);
        let p = w.build_transaction(&utxos, &dest, Some(12 * COIN), DEFAULT_FEE_RATE, &genesis()).unwrap();
        assert_eq!(p.tx.inputs().len(), 2);
        assert_eq!(p.tx.outputs[0].value, 12 * COIN);
        let (change_addr, change) = p.change.unwrap();
        assert_eq!(change_addr, w.address(Branch::Change, 0));
        assert_eq!(w.change_index, 1);
        assert_eq!(p.fee, DEFAULT_FEE_RATE * p.tx.size() as u64);
        assert_eq!(p.inputs_total, 12 * COIN + change + p.fee);
        assert_eq!(assert_valid(&p.tx, &utxos), p.fee);
    }

    #[test]
    fn dust_change_goes_to_fee() {
        let mut w = wallet();
        let utxos = vec![utxo(&w, 0, COIN, 1)];
        let dest = w.address(Branch::Receive, 2);
        let amount = COIN - 500;
        let p = w.build_transaction(&utxos, &dest, Some(amount), 1, &genesis()).unwrap();
        assert!(p.change.is_none());
        assert_eq!(p.fee, 500);
        assert_eq!(w.change_index, 0);
        assert_valid(&p.tx, &utxos);
    }

    #[test]
    fn send_all() {
        let mut w = wallet();
        let utxos = vec![utxo(&w, 0, 2 * COIN, 1), utxo(&w, 1, COIN, 2)];
        let dest = w.address(Branch::Receive, 2);
        let p = w.build_transaction(&utxos, &dest, None, 3, &genesis()).unwrap();
        assert!(p.change.is_none());
        assert_eq!(p.amount + p.fee, 3 * COIN);
        assert_valid(&p.tx, &utxos);
    }

    #[test]
    fn unusable_coins_and_errors() {
        let mut w = wallet();
        let mut immature = utxo(&w, 0, 100 * COIN, 1);
        immature.mature = false;
        let mut pending = utxo(&w, 1, 100 * COIN, 2);
        pending.spent_in_mempool = true;
        let small = utxo(&w, 2, COIN, 3);
        let utxos = vec![immature, pending, small];
        let dest = w.address(Branch::Receive, 0);
        assert!(matches!(
            w.build_transaction(&utxos, &dest, Some(2 * COIN), 1, &genesis()),
            Err(BuildError::InsufficientFunds { available, .. }) if available == COIN
        ));
        assert_eq!(w.build_transaction(&utxos, &dest, Some(1), 1, &genesis()).unwrap_err(), BuildError::AmountTooSmall);
        let other = Address::from_public_key(Network::Testnet, &w.key(Branch::Receive, 0).public_key());
        assert_eq!(
            w.build_transaction(&utxos, &other, Some(COIN / 2), 1, &genesis()).unwrap_err(),
            BuildError::WrongNetwork
        );
        assert_eq!(w.change_index, 0, "aucun index consommé en cas d'erreur");
    }

    #[test]
    fn absurd_fee_is_refused() {
        let mut w = wallet();
        let utxos = vec![utxo(&w, 0, 100 * COIN, 1)];
        let dest = w.address(Branch::Receive, 2);
        // 1 000 000 unités par octet × ~200 octets ≈ 2 KYNO : erreur de saisie.
        let err = w.build_transaction(&utxos, &dest, Some(COIN), 1_000_000, &genesis()).unwrap_err();
        assert!(matches!(err, BuildError::FeeTooHigh { fee } if fee > MAX_FEE));
        assert_eq!(w.change_index, 0, "aucun index consommé en cas d'erreur");
        // Le tarif par défaut sur la plus grande transaction possible reste très loin du plafond.
        assert!(DEFAULT_FEE_RATE * MAX_TX_SIZE as u64 * 500 <= MAX_FEE);
    }
}
