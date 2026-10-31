//! Validation des transactions et des blocs (règles de consensus).
//!
//! Trois niveaux :
//! 1. [`check_tx_structure`] / [`check_block_structure`] : vérifications sans
//!    contexte (format, tailles, montants, preuve de travail, Merkle) ;
//! 2. contexte de l'en-tête (parent, cible, horodatage) : voir `chain.rs` ;
//! 3. [`connect_block`] : vérification complète contre l'ensemble UTXO
//!    (existence des pièces, maturité, signatures, frais, récompense).

use crate::amount::{MAX_MONEY, format_amount};
use crate::block::{BLOCK_VERSION, Block, MAX_BLOCK_SIZE};
use crate::crypto::verify_signature;
use crate::hash::Hash256;
use crate::params::ConsensusParams;
use crate::tx::{MAX_COINBASE_EXTRA, MAX_TX_SIZE, OutPoint, TX_VERSION, Transaction, TxKind, TxOut};
use crate::u256::U256;
use crate::utxo::{BlockUndo, UtxoEntry, UtxoSet};
use std::collections::{HashMap, HashSet};
use std::fmt;

/// Erreur de validation d'une transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TxError {
    UnsupportedVersion(u32),
    TooLarge(usize),
    NoInputs,
    NoOutputs,
    ZeroValueOutput(usize),
    OutputTooLarge(usize),
    OutputSumTooLarge,
    DuplicateInput(OutPoint),
    CoinbaseExtraTooLarge(usize),
    /// Une transaction de récompense là où un transfert est attendu.
    UnexpectedCoinbase,
    MissingInput(OutPoint),
    ImmatureCoinbase {
        outpoint: OutPoint,
        spendable_at: u64,
    },
    WrongPublicKey(usize),
    InvalidSignature(usize),
    InputSumOverflow,
    InsufficientInputs {
        input: u64,
        output: u64,
    },
    DuplicateOutput(OutPoint),
}

impl fmt::Display for TxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TxError::UnsupportedVersion(v) => write!(f, "version de transaction non prise en charge ({v})"),
            TxError::TooLarge(s) => write!(f, "transaction trop grande ({s} octets, maximum {MAX_TX_SIZE})"),
            TxError::NoInputs => write!(f, "transaction sans entrée"),
            TxError::NoOutputs => write!(f, "transaction sans sortie"),
            TxError::ZeroValueOutput(i) => write!(f, "la sortie {i} a un montant nul"),
            TxError::OutputTooLarge(i) => write!(f, "la sortie {i} dépasse le montant maximal"),
            TxError::OutputSumTooLarge => write!(f, "la somme des sorties dépasse le montant maximal"),
            TxError::DuplicateInput(op) => write!(f, "la même pièce est dépensée deux fois ({op})"),
            TxError::CoinbaseExtraTooLarge(n) => write!(f, "données de récompense trop longues ({n} octets)"),
            TxError::UnexpectedCoinbase => write!(f, "transaction de récompense inattendue"),
            TxError::MissingInput(op) => write!(f, "pièce introuvable ou déjà dépensée ({op})"),
            TxError::ImmatureCoinbase { outpoint, spendable_at } => write!(
                f,
                "récompense de minage pas encore dépensable ({outpoint}, possible à partir du bloc {spendable_at})"
            ),
            TxError::WrongPublicKey(i) => write!(f, "l'entrée {i} ne fournit pas la bonne clé publique"),
            TxError::InvalidSignature(i) => write!(f, "signature invalide sur l'entrée {i}"),
            TxError::InputSumOverflow => write!(f, "débordement de la somme des entrées"),
            TxError::InsufficientInputs { input, output } => write!(
                f,
                "fonds insuffisants : entrées {} KYNO, sorties {} KYNO",
                format_amount(*input),
                format_amount(*output)
            ),
            TxError::DuplicateOutput(op) => write!(f, "sortie déjà existante ({op})"),
        }
    }
}

impl std::error::Error for TxError {}

/// Erreur de validation d'un bloc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockError {
    UnsupportedVersion(u32),
    TargetOutOfRange,
    InsufficientWork,
    BadMerkleRoot,
    NoTransactions,
    TooLarge(usize),
    FirstNotCoinbase,
    ExtraCoinbase(usize),
    DuplicateTransaction(Hash256),
    Transaction {
        index: usize,
        error: TxError,
    },
    BadTarget {
        expected: U256,
        found: U256,
    },
    TimestampNotIncreasing {
        parent: u64,
        found: u64,
    },
    TimestampTooFarInFuture {
        max: u64,
        found: u64,
    },
    CoinbaseHeightMismatch {
        expected: u64,
        found: Option<u64>,
    },
    CoinbaseValueTooHigh {
        max: u64,
        found: u64,
    },
    FeeOverflow,
    /// Le bloc descend d'un bloc invalide.
    InvalidAncestor(Hash256),
    /// Bloc déjà connu comme invalide.
    KnownInvalid,
    /// Erreur de stockage (disque).
    Storage(String),
    /// Le bloc est sur une branche qui ne contient pas le dernier point de
    /// contrôle signé (politique de choix de chaîne, voir `checkpoint.rs`).
    ConflictsWithCheckpoint {
        height: u64,
    },
    /// Transfert dans un bloc de la période de rodage (règle de consensus).
    TransfersLocked {
        height: u64,
        unlock_height: u64,
    },
}

impl BlockError {
    /// L'erreur tient-elle au contenu du bloc lui-même (le bloc ne sera jamais
    /// valide) ? Les autres cas (racine de Merkle ne correspondant pas — signe
    /// de données altérées en route —, horodatage trop en avance, erreur
    /// disque) ne permettent pas de conclure que l'en-tête est mauvais.
    pub fn is_permanent(&self) -> bool {
        !matches!(
            self,
            BlockError::BadMerkleRoot
                | BlockError::TimestampTooFarInFuture { .. }
                | BlockError::Storage(_)
                | BlockError::ConflictsWithCheckpoint { .. }
        )
    }

    /// Le pair qui a envoyé ce bloc s'est-il mal comporté ?
    pub fn is_peer_fault(&self) -> bool {
        // Un pair qui n'a pas encore reçu le dernier point de contrôle peut
        // envoyer de bonne foi un bloc d'une branche concurrente.
        !matches!(
            self,
            BlockError::TimestampTooFarInFuture { .. }
                | BlockError::Storage(_)
                | BlockError::ConflictsWithCheckpoint { .. }
        )
    }
}

impl fmt::Display for BlockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlockError::UnsupportedVersion(v) => write!(f, "version de bloc non prise en charge ({v})"),
            BlockError::TargetOutOfRange => write!(f, "cible hors limites"),
            BlockError::InsufficientWork => write!(f, "preuve de travail insuffisante"),
            BlockError::BadMerkleRoot => write!(f, "racine de Merkle incorrecte"),
            BlockError::NoTransactions => write!(f, "bloc sans transaction"),
            BlockError::TooLarge(s) => write!(f, "bloc trop grand ({s} octets, maximum {MAX_BLOCK_SIZE})"),
            BlockError::FirstNotCoinbase => write!(f, "la première transaction n'est pas une récompense"),
            BlockError::ExtraCoinbase(i) => write!(f, "transaction de récompense en trop (position {i})"),
            BlockError::DuplicateTransaction(id) => write!(f, "transaction en double ({id})"),
            BlockError::Transaction { index, error } => write!(f, "transaction {index} invalide : {error}"),
            BlockError::BadTarget { expected, found } => {
                write!(f, "cible incorrecte (attendue {expected}, reçue {found})")
            }
            BlockError::TimestampNotIncreasing { parent, found } => {
                write!(f, "horodatage {found} non postérieur à celui du parent ({parent})")
            }
            BlockError::TimestampTooFarInFuture { max, found } => {
                write!(f, "horodatage {found} trop loin dans le futur (maximum {max})")
            }
            BlockError::CoinbaseHeightMismatch { expected, found } => {
                write!(f, "hauteur de la récompense incorrecte (attendue {expected}, reçue {found:?})")
            }
            BlockError::CoinbaseValueTooHigh { max, found } => write!(
                f,
                "récompense trop élevée : {} KYNO réclamés, {} KYNO autorisés",
                format_amount(*found),
                format_amount(*max)
            ),
            BlockError::FeeOverflow => write!(f, "débordement du total des frais"),
            BlockError::InvalidAncestor(h) => write!(f, "descend d'un bloc invalide ({h})"),
            BlockError::ConflictsWithCheckpoint { height } => {
                write!(f, "branche incompatible avec le point de contrôle signé à la hauteur {height}")
            }
            BlockError::TransfersLocked { height, unlock_height } => write!(
                f,
                "transfert interdit au bloc {height} : période de rodage, envois permis à partir du bloc {unlock_height}"
            ),
            BlockError::KnownInvalid => write!(f, "bloc déjà identifié comme invalide"),
            BlockError::Storage(e) => write!(f, "erreur de stockage : {e}"),
        }
    }
}

impl std::error::Error for BlockError {}

/// Vérifications d'une transaction sans contexte.
pub fn check_tx_structure(tx: &Transaction) -> Result<(), TxError> {
    if tx.version != TX_VERSION {
        return Err(TxError::UnsupportedVersion(tx.version));
    }
    let size = tx.size();
    if size > MAX_TX_SIZE {
        return Err(TxError::TooLarge(size));
    }
    // Une récompense peut n'avoir aucune sortie : une fois l'émission terminée,
    // un bloc sans frais ne rapporte rien, et une sortie de montant nul est
    // interdite. Sans cette exception, plus aucun bloc vide ne serait valide.
    if tx.outputs.is_empty() && !tx.is_coinbase() {
        return Err(TxError::NoOutputs);
    }
    let mut sum: u64 = 0;
    for (i, output) in tx.outputs.iter().enumerate() {
        if output.value == 0 {
            return Err(TxError::ZeroValueOutput(i));
        }
        if output.value > MAX_MONEY {
            return Err(TxError::OutputTooLarge(i));
        }
        sum = sum.checked_add(output.value).filter(|s| *s <= MAX_MONEY).ok_or(TxError::OutputSumTooLarge)?;
    }
    match &tx.kind {
        TxKind::Coinbase { extra, .. } => {
            if extra.len() > MAX_COINBASE_EXTRA {
                return Err(TxError::CoinbaseExtraTooLarge(extra.len()));
            }
        }
        TxKind::Transfer { inputs } => {
            if inputs.is_empty() {
                return Err(TxError::NoInputs);
            }
            let mut seen = HashSet::with_capacity(inputs.len());
            for input in inputs {
                if !seen.insert(input.prev_out) {
                    return Err(TxError::DuplicateInput(input.prev_out));
                }
            }
        }
    }
    Ok(())
}

/// Vérifications d'un bloc sans contexte, dans cet ordre :
/// version, cible, preuve de travail, racine de Merkle, puis le reste.
///
/// La racine de Merkle est vérifiée avant toute règle portant sur le contenu :
/// si elle ne correspond pas, les transactions ont pu être altérées en route et
/// l'en-tête lui-même n'est pas en cause.
pub fn check_block_structure(block: &Block, params: &ConsensusParams) -> Result<(), BlockError> {
    let header = &block.header;
    if header.version != BLOCK_VERSION {
        return Err(BlockError::UnsupportedVersion(header.version));
    }
    if header.target.is_zero() || header.target > params.pow_limit {
        return Err(BlockError::TargetOutOfRange);
    }
    if !header.meets_target() {
        return Err(BlockError::InsufficientWork);
    }
    if block.compute_merkle_root() != header.merkle_root {
        return Err(BlockError::BadMerkleRoot);
    }
    if block.transactions.is_empty() {
        return Err(BlockError::NoTransactions);
    }
    let size = block.size();
    if size > MAX_BLOCK_SIZE {
        return Err(BlockError::TooLarge(size));
    }
    if !block.transactions[0].is_coinbase() {
        return Err(BlockError::FirstNotCoinbase);
    }
    let mut seen = HashSet::with_capacity(block.transactions.len());
    for (index, tx) in block.transactions.iter().enumerate() {
        if index > 0 && tx.is_coinbase() {
            return Err(BlockError::ExtraCoinbase(index));
        }
        let txid = tx.txid();
        if !seen.insert(txid) {
            return Err(BlockError::DuplicateTransaction(txid));
        }
        check_tx_structure(tx).map_err(|error| BlockError::Transaction { index, error })?;
    }
    Ok(())
}

/// Vérifie les entrées d'un transfert contre une vue de l'ensemble UTXO et
/// renvoie les frais. `spend_height` est la hauteur du bloc qui inclura la
/// transaction.
pub fn check_tx_inputs<F>(
    tx: &Transaction,
    lookup: F,
    spend_height: u64,
    params: &ConsensusParams,
    genesis_hash: &Hash256,
) -> Result<u64, TxError>
where
    F: Fn(&OutPoint) -> Option<UtxoEntry>,
{
    let (fee, spent) = check_tx_inputs_unsigned(tx, lookup, spend_height, params)?;
    verify_input_signatures(tx, &spent, genesis_hash)?;
    Ok(fee)
}

/// Toutes les vérifications des entrées **sauf les signatures** (les plus
/// coûteuses) : existence, maturité, clé publique, sommes. Renvoie les frais
/// et les sorties dépensées (nécessaires au calcul de l'empreinte de
/// signature). Permet de refuser à bas coût une transaction qui échouerait
/// de toute façon (pièce inconnue, frais insuffisants…) avant de vérifier
/// ses signatures.
pub fn check_tx_inputs_unsigned<F>(
    tx: &Transaction,
    lookup: F,
    spend_height: u64,
    params: &ConsensusParams,
) -> Result<(u64, Vec<TxOut>), TxError>
where
    F: Fn(&OutPoint) -> Option<UtxoEntry>,
{
    if tx.is_coinbase() {
        return Err(TxError::UnexpectedCoinbase);
    }
    let inputs = tx.inputs();
    let mut spent = Vec::with_capacity(inputs.len());
    let mut total_in: u64 = 0;
    for (i, input) in inputs.iter().enumerate() {
        let entry = lookup(&input.prev_out).ok_or(TxError::MissingInput(input.prev_out))?;
        if !entry.is_mature(spend_height, params.coinbase_maturity) {
            return Err(TxError::ImmatureCoinbase {
                outpoint: input.prev_out,
                spendable_at: entry.height.saturating_add(params.coinbase_maturity),
            });
        }
        if input.public_key.pubkey_hash() != entry.output.recipient {
            return Err(TxError::WrongPublicKey(i));
        }
        total_in = total_in.checked_add(entry.output.value).ok_or(TxError::InputSumOverflow)?;
        spent.push(entry.output);
    }
    let total_out = tx.output_sum().ok_or(TxError::OutputSumTooLarge)?;
    if total_in < total_out {
        return Err(TxError::InsufficientInputs { input: total_in, output: total_out });
    }
    Ok((total_in - total_out, spent))
}

/// Vérifie la signature de chaque entrée. `spent` : les sorties dépensées,
/// dans l'ordre des entrées (voir [`check_tx_inputs_unsigned`]).
pub fn verify_input_signatures(tx: &Transaction, spent: &[TxOut], genesis_hash: &Hash256) -> Result<(), TxError> {
    let sighash = tx.sighash(genesis_hash, spent);
    for (i, input) in tx.inputs().iter().enumerate() {
        if !verify_signature(&input.public_key, &sighash.0, &input.signature) {
            return Err(TxError::InvalidSignature(i));
        }
    }
    Ok(())
}

/// Applique un bloc (déjà validé sans contexte) à l'ensemble UTXO après
/// vérification complète. En cas d'erreur, l'ensemble UTXO n'est **pas**
/// modifié. Renvoie les données d'annulation.
/// Période de rodage : avant `transfers_unlock_height`, un bloc ne contient que
/// sa récompense de minage.
pub fn check_transfers_allowed(block: &Block, height: u64, params: &ConsensusParams) -> Result<(), BlockError> {
    if block.transactions.len() > 1 && !params.transfers_allowed(height) {
        return Err(BlockError::TransfersLocked { height, unlock_height: params.transfers_unlock_height });
    }
    Ok(())
}

pub fn connect_block(
    utxo: &mut UtxoSet,
    block: &Block,
    height: u64,
    params: &ConsensusParams,
    genesis_hash: &Hash256,
) -> Result<BlockUndo, BlockError> {
    let coinbase = block.coinbase().ok_or(BlockError::FirstNotCoinbase)?;
    if coinbase.coinbase_height() != Some(height) {
        return Err(BlockError::CoinbaseHeightMismatch { expected: height, found: coinbase.coinbase_height() });
    }
    check_transfers_allowed(block, height, params)?;

    // Vue temporaire : rien n'est écrit dans `utxo` avant la fin.
    let mut created: HashMap<OutPoint, UtxoEntry> = HashMap::new();
    let mut spent_set: HashSet<OutPoint> = HashSet::new();
    let mut spent_from_base: Vec<(OutPoint, UtxoEntry)> = Vec::new();
    let mut fees: u64 = 0;

    for (index, tx) in block.transactions.iter().enumerate().skip(1) {
        let fee = {
            let lookup = |op: &OutPoint| -> Option<UtxoEntry> {
                if spent_set.contains(op) {
                    return None;
                }
                created.get(op).copied().or_else(|| utxo.get(op).copied())
            };
            check_tx_inputs(tx, lookup, height, params, genesis_hash)
                .map_err(|error| BlockError::Transaction { index, error })?
        };
        fees = fees.checked_add(fee).ok_or(BlockError::FeeOverflow)?;
        for input in tx.inputs() {
            let op = input.prev_out;
            if created.remove(&op).is_none() {
                if let Some(entry) = utxo.get(&op) {
                    spent_from_base.push((op, *entry));
                }
                spent_set.insert(op);
            }
        }
        add_outputs(&mut created, utxo, &spent_set, tx, height, false)
            .map_err(|error| BlockError::Transaction { index, error })?;
    }

    let max = params.block_subsidy(height).checked_add(fees).ok_or(BlockError::FeeOverflow)?;
    let claimed = coinbase.output_sum().ok_or(BlockError::FeeOverflow)?;
    if claimed > max {
        return Err(BlockError::CoinbaseValueTooHigh { max, found: claimed });
    }
    add_outputs(&mut created, utxo, &spent_set, coinbase, height, true)
        .map_err(|error| BlockError::Transaction { index: 0, error })?;

    // Validation réussie : on applique.
    for (op, _) in &spent_from_base {
        utxo.remove(op);
    }
    for (op, entry) in created {
        utxo.insert(op, entry);
    }
    Ok(BlockUndo { spent: spent_from_base })
}

fn add_outputs(
    created: &mut HashMap<OutPoint, UtxoEntry>,
    utxo: &UtxoSet,
    spent_set: &HashSet<OutPoint>,
    tx: &Transaction,
    height: u64,
    is_coinbase: bool,
) -> Result<(), TxError> {
    let txid = tx.txid();
    for (vout, output) in tx.outputs.iter().enumerate() {
        let op = OutPoint { txid, vout: vout as u32 };
        let exists = created.contains_key(&op) || (utxo.contains(&op) && !spent_set.contains(&op));
        if exists {
            return Err(TxError::DuplicateOutput(op));
        }
        created.insert(op, UtxoEntry { output: *output, height, is_coinbase });
    }
    Ok(())
}

/// Annule un bloc (le dernier connecté) grâce à ses données d'annulation.
pub fn disconnect_block(utxo: &mut UtxoSet, block: &Block, undo: &BlockUndo) {
    for tx in block.transactions.iter().rev() {
        let txid = tx.txid();
        for vout in 0..tx.outputs.len() {
            utxo.remove(&OutPoint { txid, vout: vout as u32 });
        }
    }
    for (op, entry) in &undo.spent {
        utxo.insert(*op, *entry);
    }
}
