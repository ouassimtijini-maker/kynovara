//! Transactions (modèle UTXO, comme Bitcoin).
//!
//! Une transaction dépense des sorties existantes (ses entrées) et crée de
//! nouvelles sorties. Chaque entrée fournit la clé publique dont l'empreinte
//! verrouille la sortie dépensée, et une signature Ed25519 de l'empreinte de
//! signature (« sighash »).
//!
//! Identifiants :
//! - `txid` : hachage de la transaction **sans** les signatures. Les
//!   références entre transactions utilisent le txid, qui ne peut donc pas être
//!   altéré en modifiant une signature ;
//! - `wtxid` : hachage **avec** les signatures, engagé dans l'arbre de Merkle
//!   du bloc (le bloc engage ainsi la totalité des octets).

use crate::crypto::{PUBLIC_KEY_SIZE, PubKeyHash, PublicKey, SIGNATURE_SIZE, Signature};
use crate::encode::{Decode, DecodeError, Encode, Reader, put_len, put_u8, put_u32, put_u64};
use crate::hash::{Hash256, tagged_hash, tags};
use std::fmt;

/// Version de transaction acceptée (règle de consensus).
pub const TX_VERSION: u32 = 1;
/// Taille maximale d'une transaction encodée (octets).
pub const MAX_TX_SIZE: usize = 100_000;
/// Nombre maximal d'entrées (borne de décodage).
pub const MAX_TX_INPUTS: usize = 1_000;
/// Nombre maximal de sorties (borne de décodage).
pub const MAX_TX_OUTPUTS: usize = 2_000;
/// Taille maximale des données libres d'une transaction de récompense.
pub const MAX_COINBASE_EXTRA: usize = 64;

const TAG_COINBASE: u8 = 0;
const TAG_TRANSFER: u8 = 1;
/// Taille d'une entrée encodée (txid + index + clé publique + signature).
pub const INPUT_SIZE: usize = 32 + 4 + PUBLIC_KEY_SIZE + SIGNATURE_SIZE;
/// Taille d'une sortie encodée (montant + empreinte de clé).
pub const OUTPUT_SIZE: usize = 8 + 20;

/// Référence à une sortie : (txid, numéro de sortie).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OutPoint {
    pub txid: Hash256,
    pub vout: u32,
}

impl fmt::Debug for OutPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.txid, self.vout)
    }
}

impl fmt::Display for OutPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.txid, self.vout)
    }
}

/// Entrée : dépense de la sortie `prev_out`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TxIn {
    pub prev_out: OutPoint,
    pub public_key: PublicKey,
    pub signature: Signature,
}

/// Sortie : `value` unités verrouillées par l'empreinte `recipient`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TxOut {
    pub value: u64,
    pub recipient: PubKeyHash,
}

/// Nature de la transaction.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TxKind {
    /// Transaction de récompense (première transaction de chaque bloc). Elle
    /// engage la hauteur du bloc, ce qui rend son identifiant unique.
    Coinbase { height: u64, extra: Vec<u8> },
    /// Transfert ordinaire.
    Transfer { inputs: Vec<TxIn> },
}

/// Transaction.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Transaction {
    pub version: u32,
    pub kind: TxKind,
    pub outputs: Vec<TxOut>,
}

impl Transaction {
    /// Construit une transaction de récompense.
    pub fn coinbase(height: u64, extra: Vec<u8>, outputs: Vec<TxOut>) -> Self {
        Transaction { version: TX_VERSION, kind: TxKind::Coinbase { height, extra }, outputs }
    }

    /// Construit un transfert.
    pub fn transfer(inputs: Vec<TxIn>, outputs: Vec<TxOut>) -> Self {
        Transaction { version: TX_VERSION, kind: TxKind::Transfer { inputs }, outputs }
    }

    pub fn is_coinbase(&self) -> bool {
        matches!(self.kind, TxKind::Coinbase { .. })
    }

    /// Entrées (vide pour une transaction de récompense).
    pub fn inputs(&self) -> &[TxIn] {
        match &self.kind {
            TxKind::Coinbase { .. } => &[],
            TxKind::Transfer { inputs } => inputs,
        }
    }

    /// Hauteur engagée par une transaction de récompense.
    pub fn coinbase_height(&self) -> Option<u64> {
        match &self.kind {
            TxKind::Coinbase { height, .. } => Some(*height),
            TxKind::Transfer { .. } => None,
        }
    }

    fn encode_inner(&self, out: &mut Vec<u8>, with_signatures: bool) {
        put_u32(out, self.version);
        match &self.kind {
            TxKind::Coinbase { height, extra } => {
                put_u8(out, TAG_COINBASE);
                put_u64(out, *height);
                put_len(out, extra.len());
                out.extend_from_slice(extra);
            }
            TxKind::Transfer { inputs } => {
                put_u8(out, TAG_TRANSFER);
                put_len(out, inputs.len());
                for input in inputs {
                    out.extend_from_slice(&input.prev_out.txid.0);
                    put_u32(out, input.prev_out.vout);
                    out.extend_from_slice(&input.public_key.0);
                    if with_signatures {
                        out.extend_from_slice(&input.signature.0);
                    }
                }
            }
        }
        put_len(out, self.outputs.len());
        for output in &self.outputs {
            put_u64(out, output.value);
            out.extend_from_slice(&output.recipient.0);
        }
    }

    /// Encodage sans signatures.
    pub fn encode_without_signatures(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.size());
        self.encode_inner(&mut out, false);
        out
    }

    /// Identifiant (sans les signatures).
    pub fn txid(&self) -> Hash256 {
        tagged_hash(tags::TXID, &[&self.encode_without_signatures()])
    }

    /// Identifiant engageant aussi les signatures.
    pub fn wtxid(&self) -> Hash256 {
        tagged_hash(tags::WTXID, &[&self.to_bytes()])
    }

    /// Taille encodée complète, en octets.
    pub fn size(&self) -> usize {
        let kind = match &self.kind {
            TxKind::Coinbase { extra, .. } => 8 + 4 + extra.len(),
            TxKind::Transfer { inputs } => 4 + inputs.len() * INPUT_SIZE,
        };
        4 + 1 + kind + 4 + self.outputs.len() * OUTPUT_SIZE
    }

    /// Somme des sorties (None en cas de débordement).
    pub fn output_sum(&self) -> Option<u64> {
        self.outputs.iter().try_fold(0u64, |acc, o| acc.checked_add(o.value))
    }

    /// Empreinte de signature (règle de consensus).
    ///
    /// Elle engage : l'empreinte du bloc de genèse (une signature d'un réseau
    /// est inutilisable sur un autre), toute la transaction sauf les
    /// signatures, et le montant + destinataire de chaque sortie dépensée (un
    /// portefeuille matériel ne peut donc pas être trompé sur les frais).
    pub fn sighash(&self, genesis_hash: &Hash256, spent_outputs: &[TxOut]) -> Hash256 {
        let mut buf = Vec::with_capacity(32 + self.size() + 4 + spent_outputs.len() * OUTPUT_SIZE);
        buf.extend_from_slice(&genesis_hash.0);
        self.encode_inner(&mut buf, false);
        put_len(&mut buf, spent_outputs.len());
        for output in spent_outputs {
            put_u64(&mut buf, output.value);
            buf.extend_from_slice(&output.recipient.0);
        }
        tagged_hash(tags::SIGHASH, &[&buf])
    }
}

impl Encode for Transaction {
    fn encode_to(&self, out: &mut Vec<u8>) {
        self.encode_inner(out, true);
    }
}

impl Decode for Transaction {
    fn decode_from(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let version = r.read_u32()?;
        let kind = match r.read_u8()? {
            TAG_COINBASE => {
                let height = r.read_u64()?;
                let len = r.read_len(MAX_COINBASE_EXTRA, 1)?;
                let extra = r.read_bytes(len)?.to_vec();
                TxKind::Coinbase { height, extra }
            }
            TAG_TRANSFER => {
                let count = r.read_len(MAX_TX_INPUTS, INPUT_SIZE)?;
                let mut inputs = Vec::with_capacity(count);
                for _ in 0..count {
                    let txid = Hash256(r.read_array()?);
                    let vout = r.read_u32()?;
                    let public_key = PublicKey(r.read_array()?);
                    let signature = Signature(r.read_array()?);
                    inputs.push(TxIn { prev_out: OutPoint { txid, vout }, public_key, signature });
                }
                TxKind::Transfer { inputs }
            }
            _ => return Err(DecodeError::InvalidValue("type de transaction inconnu")),
        };
        let count = r.read_len(MAX_TX_OUTPUTS, OUTPUT_SIZE)?;
        let mut outputs = Vec::with_capacity(count);
        for _ in 0..count {
            let value = r.read_u64()?;
            let recipient = PubKeyHash(r.read_array()?);
            outputs.push(TxOut { value, recipient });
        }
        Ok(Transaction { version, kind, outputs })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::KeyPair;

    fn sample() -> Transaction {
        let kp = KeyPair::from_secret_bytes(&[5u8; 32]);
        Transaction::transfer(
            vec![TxIn {
                prev_out: OutPoint { txid: Hash256([9; 32]), vout: 3 },
                public_key: kp.public_key(),
                signature: Signature([7; 64]),
            }],
            vec![
                TxOut { value: 1000, recipient: PubKeyHash([1; 20]) },
                TxOut { value: 2000, recipient: PubKeyHash([2; 20]) },
            ],
        )
    }

    #[test]
    fn encode_roundtrip_and_size() {
        let tx = sample();
        let bytes = tx.to_bytes();
        assert_eq!(bytes.len(), tx.size());
        assert_eq!(Transaction::from_bytes(&bytes).unwrap(), tx);
        let cb =
            Transaction::coinbase(5, b"bonjour".to_vec(), vec![TxOut { value: 1, recipient: PubKeyHash([3; 20]) }]);
        let bytes = cb.to_bytes();
        assert_eq!(bytes.len(), cb.size());
        assert_eq!(Transaction::from_bytes(&bytes).unwrap(), cb);
    }

    #[test]
    fn txid_ignores_signatures_but_wtxid_does_not() {
        let a = sample();
        let mut b = sample();
        if let TxKind::Transfer { inputs } = &mut b.kind {
            inputs[0].signature = Signature([8; 64]);
        }
        assert_eq!(a.txid(), b.txid());
        assert_ne!(a.wtxid(), b.wtxid());
    }

    #[test]
    fn sighash_commits_to_everything() {
        let tx = sample();
        let spent = [TxOut { value: 5000, recipient: PubKeyHash([4; 20]) }];
        let g = Hash256([0xaa; 32]);
        let base = tx.sighash(&g, &spent);
        // Autre réseau.
        assert_ne!(base, tx.sighash(&Hash256([0xbb; 32]), &spent));
        // Autre montant dépensé.
        assert_ne!(base, tx.sighash(&g, &[TxOut { value: 5001, ..spent[0] }]));
        // Sortie modifiée.
        let mut t2 = tx.clone();
        t2.outputs[0].value += 1;
        assert_ne!(base, t2.sighash(&g, &spent));
        // La signature elle-même n'est pas engagée.
        let mut t3 = tx.clone();
        if let TxKind::Transfer { inputs } = &mut t3.kind {
            inputs[0].signature = Signature([0; 64]);
        }
        assert_eq!(base, t3.sighash(&g, &spent));
    }

    #[test]
    fn decode_rejects_garbage() {
        let bytes = sample().to_bytes();
        // Tronqué à chaque position possible.
        for cut in 0..bytes.len() {
            assert!(Transaction::from_bytes(&bytes[..cut]).is_err());
        }
        // Octet en trop.
        let mut extra = bytes.clone();
        extra.push(0);
        assert_eq!(Transaction::from_bytes(&extra), Err(DecodeError::TrailingBytes(1)));
        // Type inconnu.
        let mut bad = bytes;
        bad[4] = 2;
        assert!(Transaction::from_bytes(&bad).is_err());
    }
}
