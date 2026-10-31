//! Signatures Ed25519 et empreintes de clés publiques.
//!
//! Règle de consensus : une signature est valide si et seulement si
//! `ed25519_dalek::VerifyingKey::verify_strict` l'accepte et que la clé
//! publique n'est pas « faible » (d'ordre faible). La vérification stricte
//! refuse les signatures non canoniques : un tiers ne peut donc pas modifier
//! une signature valide pour en fabriquer une autre (non-malléabilité).

use crate::hash::{tagged_hash, tags};
use crate::hex;
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use std::fmt;

/// Taille d'une clé publique Ed25519.
pub const PUBLIC_KEY_SIZE: usize = 32;
/// Taille d'une signature Ed25519.
pub const SIGNATURE_SIZE: usize = 64;
/// Taille de l'empreinte d'une clé publique (contenu d'une adresse).
pub const PUBKEY_HASH_SIZE: usize = 20;

/// Clé publique Ed25519 (32 octets, non validée tant qu'on ne vérifie pas
/// une signature avec).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PublicKey(pub [u8; PUBLIC_KEY_SIZE]);

impl PublicKey {
    /// Empreinte de la clé : les 20 premiers octets du hachage étiqueté.
    pub fn pubkey_hash(&self) -> PubKeyHash {
        let h = tagged_hash(tags::PUBKEY_HASH, &[&self.0]);
        let mut out = [0u8; PUBKEY_HASH_SIZE];
        out.copy_from_slice(&h.0[..PUBKEY_HASH_SIZE]);
        PubKeyHash(out)
    }

    pub fn to_hex(&self) -> String {
        hex::encode(&self.0)
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({})", self.to_hex())
    }
}

/// Signature Ed25519 (64 octets).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Signature(pub [u8; SIGNATURE_SIZE]);

impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Signature({})", hex::encode(&self.0))
    }
}

/// Empreinte de clé publique : c'est ce que contient une adresse et ce que
/// « verrouille » une sortie de transaction.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct PubKeyHash(pub [u8; PUBKEY_HASH_SIZE]);

impl PubKeyHash {
    pub fn to_hex(&self) -> String {
        hex::encode(&self.0)
    }
}

impl fmt::Debug for PubKeyHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PubKeyHash({})", self.to_hex())
    }
}

impl fmt::Display for PubKeyHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// Vérifie une signature (règle de consensus, voir la documentation du module).
pub fn verify_signature(public_key: &PublicKey, message: &[u8; 32], signature: &Signature) -> bool {
    let Ok(key) = VerifyingKey::from_bytes(&public_key.0) else {
        return false;
    };
    if key.is_weak() {
        return false;
    }
    let sig = ed25519_dalek::Signature::from_bytes(&signature.0);
    key.verify_strict(message, &sig).is_ok()
}

/// La clé publique est-elle un point Ed25519 utilisable (décodable, non faible) ?
pub fn is_valid_public_key(public_key: &PublicKey) -> bool {
    VerifyingKey::from_bytes(&public_key.0).is_ok_and(|k| !k.is_weak())
}

/// Paire de clés. La clé secrète est effacée de la mémoire à la destruction.
pub struct KeyPair {
    signing: SigningKey,
}

impl KeyPair {
    /// Construit une paire de clés à partir d'une clé secrète de 32 octets
    /// (graine Ed25519 au sens de la RFC 8032).
    pub fn from_secret_bytes(secret: &[u8; 32]) -> Self {
        KeyPair { signing: SigningKey::from_bytes(secret) }
    }

    pub fn public_key(&self) -> PublicKey {
        PublicKey(self.signing.verifying_key().to_bytes())
    }

    /// Signe un message de 32 octets (une empreinte de signature de transaction).
    pub fn sign(&self, message: &[u8; 32]) -> Signature {
        Signature(self.signing.sign(message).to_bytes())
    }
}

impl fmt::Debug for KeyPair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyPair").field("public_key", &self.public_key()).field("secret", &"<masquée>").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 8032, section 7.1, TEST 1 (message vide) et TEST 2 (message 0x72).
    const SK1: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
    const PK1: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
    const SIG1: &str = "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b";
    const SK2: &str = "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb";
    const PK2: &str = "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c";
    const SIG2: &str = "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00";

    #[test]
    fn rfc8032_vectors() {
        for (sk, pk, sig, msg) in [(SK1, PK1, SIG1, &b""[..]), (SK2, PK2, SIG2, &[0x72u8][..])] {
            let signing = SigningKey::from_bytes(&hex::decode_array(sk).unwrap());
            assert_eq!(hex::encode(&signing.verifying_key().to_bytes()), pk);
            assert_eq!(hex::encode(&signing.sign(msg).to_bytes()), sig);
        }
    }

    #[test]
    fn sign_and_verify() {
        let kp = KeyPair::from_secret_bytes(&[7u8; 32]);
        let msg = [42u8; 32];
        let sig = kp.sign(&msg);
        assert!(verify_signature(&kp.public_key(), &msg, &sig));
        // Mauvais message, mauvaise clé, signature altérée.
        assert!(!verify_signature(&kp.public_key(), &[43u8; 32], &sig));
        let other = KeyPair::from_secret_bytes(&[8u8; 32]);
        assert!(!verify_signature(&other.public_key(), &msg, &sig));
        let mut bad = sig;
        bad.0[10] ^= 1;
        assert!(!verify_signature(&kp.public_key(), &msg, &bad));
    }

    #[test]
    fn non_canonical_s_is_rejected() {
        // S + L (ordre du groupe) donne une signature « malléable » que la
        // vérification stricte doit refuser.
        const L: [u8; 32] = [
            0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10,
        ];
        let kp = KeyPair::from_secret_bytes(&[9u8; 32]);
        let msg = [1u8; 32];
        let sig = kp.sign(&msg);
        let mut s = [0u8; 32];
        s.copy_from_slice(&sig.0[32..]);
        // Addition petit-boutiste s + L.
        let mut carry = 0u16;
        for i in 0..32 {
            let v = u16::from(s[i]) + u16::from(L[i]) + carry;
            s[i] = v as u8;
            carry = v >> 8;
        }
        let mut malleated = sig;
        malleated.0[32..].copy_from_slice(&s);
        assert!(!verify_signature(&kp.public_key(), &msg, &malleated));
    }

    #[test]
    fn weak_public_key_is_rejected() {
        // L'identité du groupe (point d'ordre 1) : n'importe quelle signature
        // de la forme (R = identité, S = 0) « vérifierait » sans contrôle.
        let mut identity = [0u8; 32];
        identity[0] = 1;
        let mut sig = [0u8; 64];
        sig[0] = 1;
        assert!(!verify_signature(&PublicKey(identity), &[0u8; 32], &Signature(sig)));
        // Octets qui ne décodent pas en point valide.
        assert!(!verify_signature(&PublicKey([0xff; 32]), &[0u8; 32], &Signature([0; 64])));
    }

    #[test]
    fn pubkey_hash_is_stable() {
        let kp = KeyPair::from_secret_bytes(&[3u8; 32]);
        assert_eq!(kp.public_key().pubkey_hash(), kp.public_key().pubkey_hash());
        let other = KeyPair::from_secret_bytes(&[4u8; 32]);
        assert_ne!(kp.public_key().pubkey_hash(), other.public_key().pubkey_hash());
    }
}
