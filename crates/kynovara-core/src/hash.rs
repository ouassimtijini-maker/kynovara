//! Hachage : SHA-256, double SHA-256 et hachage « étiqueté ».
//!
//! Le hachage étiqueté (même construction que BIP-340) sépare les domaines :
//! un identifiant de transaction ne peut jamais être confondu avec un nœud
//! d'arbre de Merkle ou un message signé, même si les octets hachés sont
//! identiques.

use crate::hex::{self, HexError};
use sha2::{Digest, Sha256};
use std::fmt;
use std::str::FromStr;

/// Empreinte de 32 octets (identifiant de bloc, de transaction, etc.).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Hash256(pub [u8; 32]);

impl Hash256 {
    /// Empreinte nulle (32 octets à zéro).
    pub const ZERO: Hash256 = Hash256([0; 32]);

    /// Construit une empreinte à partir de 32 octets.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Hash256(bytes)
    }

    /// Lit une empreinte hexadécimale à la compilation (constantes).
    ///
    /// # Panique
    /// À la compilation si le texte n'est pas une empreinte hexadécimale de
    /// 64 caractères.
    pub const fn from_hex_const(s: &str) -> Self {
        let b = s.as_bytes();
        assert!(b.len() == 64, "une empreinte fait 64 caractères hexadécimaux");
        let mut out = [0u8; 32];
        let mut i = 0;
        while i < 32 {
            out[i] = (const_nibble(b[2 * i]) << 4) | const_nibble(b[2 * i + 1]);
            i += 1;
        }
        Hash256(out)
    }

    /// Les 32 octets de l'empreinte.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Représentation hexadécimale (64 caractères).
    pub fn to_hex(&self) -> String {
        hex::encode(&self.0)
    }

    /// Lit une empreinte hexadécimale de 64 caractères.
    pub fn from_hex(s: &str) -> Result<Self, HexError> {
        Ok(Hash256(hex::decode_array(s)?))
    }
}

const fn const_nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        _ => panic!("caractère hexadécimal minuscule attendu"),
    }
}

impl fmt::Display for Hash256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Hash256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hash256({})", self.to_hex())
    }
}

impl FromStr for Hash256 {
    type Err = HexError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Hash256::from_hex(s)
    }
}

/// SHA-256 simple.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// Double SHA-256 (utilisé pour la preuve de travail, comme Bitcoin).
pub fn sha256d(data: &[u8]) -> Hash256 {
    Hash256(sha256(&sha256(data)))
}

/// Hachage étiqueté : `SHA256(SHA256(étiquette) || SHA256(étiquette) || données…)`.
pub fn tagged_hash(tag: &str, parts: &[&[u8]]) -> Hash256 {
    let tag_hash = sha256(tag.as_bytes());
    let mut h = Sha256::new();
    h.update(tag_hash);
    h.update(tag_hash);
    for part in parts {
        h.update(part);
    }
    Hash256(h.finalize().into())
}

/// Étiquettes de séparation de domaine (règles de consensus).
pub mod tags {
    /// Identifiant de transaction (sans les signatures).
    pub const TXID: &str = "Kynovara/txid";
    /// Identifiant de transaction avec signatures (engagé dans l'arbre de Merkle).
    pub const WTXID: &str = "Kynovara/wtxid";
    /// Message signé par les entrées d'une transaction.
    pub const SIGHASH: &str = "Kynovara/sighash";
    /// Feuille de l'arbre de Merkle.
    pub const MERKLE_LEAF: &str = "Kynovara/merkle-leaf";
    /// Nœud interne de l'arbre de Merkle.
    pub const MERKLE_NODE: &str = "Kynovara/merkle-node";
    /// Empreinte d'une clé publique (contenu d'une adresse).
    pub const PUBKEY_HASH: &str = "Kynovara/pubkey-hash";
    /// Point de contrôle signé.
    pub const CHECKPOINT: &str = "Kynovara/checkpoint";
    /// Délégation d'une clé de signature par la clé maîtresse.
    pub const DELEGATION: &str = "Kynovara/delegation";
    /// Révocation d'une clé de signature.
    pub const REVOCATION: &str = "Kynovara/revocation";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vectors() {
        // Vecteurs FIPS 180-2.
        assert_eq!(hex::encode(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(hex::encode(&sha256(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256d(b"abc").to_hex(), "4f8b42c22dd3729b519ba6f68d2da7cc5b2d606d05daed5ad5128cc03e6c6358");
    }

    #[test]
    fn tagged_hash_separates_domains() {
        let a = tagged_hash(tags::TXID, &[b"donnees"]);
        let b = tagged_hash(tags::WTXID, &[b"donnees"]);
        assert_ne!(a, b);
        // Le découpage en morceaux ne change pas le résultat.
        assert_eq!(a, tagged_hash(tags::TXID, &[b"don", b"nees"]));
    }

    #[test]
    fn hex_roundtrip() {
        let h = sha256d(b"kynovara");
        assert_eq!(Hash256::from_hex(&h.to_hex()).unwrap(), h);
        assert!(Hash256::from_hex("00").is_err());
    }
}
