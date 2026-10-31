//! Points de contrôle signés : la protection de lancement contre les
//! réécritures de l'historique (attaque des 51 %).
//!
//! Principe (détails dans docs/SPECIFICATION.md, § 12) :
//! - une **clé maîtresse**, inscrite dans les paramètres du réseau, ne signe
//!   presque jamais : elle autorise des clés de signature pour une durée
//!   limitée (délégations), les révoque et, en dernier recours, signe
//!   elle-même un point de contrôle pour résoudre une alerte. Elle est
//!   dérivée d'une phrase de 24 mots gardée sur papier, hors ligne ;
//! - une **clé de signature** (déléguée) installée sur le nœud du fondateur
//!   signe chaque nouveau sommet de sa chaîne ;
//! - les nœuds refusent toute branche qui ne contient pas le dernier point de
//!   contrôle : même avec la majorité de la puissance de calcul, un attaquant
//!   ne peut plus réécrire ce qui le précède ;
//! - deux points contradictoires signés (clé de signature volée ?)
//!   déclenchent une alerte : les nœuds gèlent leur chaîne jusqu'à ce que la
//!   clé maîtresse tranche ;
//! - au-delà de `checkpoint_sunset_height`, plus aucun point n'est accepté :
//!   le réseau redevient purement fondé sur la preuve de travail.
//!
//! Ce n'est pas une règle de validité des blocs mais une règle de **choix de
//! la chaîne**. Chaque signature couvre l'empreinte de la genèse : un point
//! d'un réseau n'est jamais valable sur un autre.

use crate::crypto::{KeyPair, PUBLIC_KEY_SIZE, PublicKey, SIGNATURE_SIZE, Signature, verify_signature};
use crate::encode::{Decode, DecodeError, Encode, Reader, put_u8, put_u64};
use crate::hash::{Hash256, tagged_hash, tags};
use crate::params::ConsensusParams;
use std::collections::HashSet;
use std::fmt;

/// Durée de validité conseillée d'une délégation : environ 90 jours de blocs.
pub const DEFAULT_DELEGATION_BLOCKS: u64 = 64_800;

/// Chemin de dérivation (SLIP-0010, tous durcis) de la clé maîtresse à partir
/// de sa phrase de 24 mots : `m / 44' / type' / 9000' / 0' / 0'`. Il est
/// distinct de celui des portefeuilles (compte `0'`).
pub const MASTER_KEY_ACCOUNT: u32 = 9000;

fn digest(tag: &str, genesis: &Hash256, parts: &[&[u8]]) -> [u8; 32] {
    let mut all: Vec<&[u8]> = vec![genesis.as_bytes()];
    all.extend_from_slice(parts);
    tagged_hash(tag, &all).0
}

/// Erreur de vérification d'un point de contrôle ou d'une révocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckpointError {
    /// Le réseau n'a pas de clé maîtresse : les points de contrôle sont désactivés.
    Disabled,
    /// Point au-delà de la hauteur de fin des points de contrôle.
    AfterSunset { height: u64, sunset: u64 },
    /// Délégation non signée par la clé maîtresse.
    BadDelegation,
    /// Délégation expirée pour cette hauteur.
    DelegationExpired { height: u64, max_height: u64 },
    /// Clé de signature révoquée.
    Revoked(PublicKey),
    /// Signature fausse.
    BadSignature,
}

impl fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CheckpointError::Disabled => write!(f, "points de contrôle désactivés sur ce réseau"),
            CheckpointError::AfterSunset { height, sunset } => {
                write!(f, "hauteur {height} au-delà de la fin des points de contrôle ({sunset})")
            }
            CheckpointError::BadDelegation => write!(f, "délégation non signée par la clé maîtresse"),
            CheckpointError::DelegationExpired { height, max_height } => {
                write!(f, "délégation expirée (hauteur {height}, valable jusqu'à {max_height})")
            }
            CheckpointError::Revoked(k) => write!(f, "clé de signature révoquée ({})", k.to_hex()),
            CheckpointError::BadSignature => write!(f, "signature invalide"),
        }
    }
}

impl std::error::Error for CheckpointError {}

fn master_key(params: &ConsensusParams) -> Result<PublicKey, CheckpointError> {
    params.checkpoint_master.ok_or(CheckpointError::Disabled)
}

/// Autorisation donnée par la clé maîtresse à une clé de signature, jusqu'à
/// la hauteur `max_height` incluse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Delegation {
    pub signer: PublicKey,
    pub max_height: u64,
    pub signature: Signature,
}

impl Delegation {
    fn digest(genesis: &Hash256, signer: &PublicKey, max_height: u64) -> [u8; 32] {
        digest(tags::DELEGATION, genesis, &[&signer.0, &max_height.to_le_bytes()])
    }

    pub fn new(master: &KeyPair, genesis: &Hash256, signer: PublicKey, max_height: u64) -> Self {
        let signature = master.sign(&Self::digest(genesis, &signer, max_height));
        Delegation { signer, max_height, signature }
    }

    pub fn verify(&self, master: &PublicKey, genesis: &Hash256) -> bool {
        verify_signature(master, &Self::digest(genesis, &self.signer, self.max_height), &self.signature)
    }
}

/// Qui a signé un point de contrôle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Authority {
    /// La clé maîtresse elle-même (résolution d'une alerte).
    Master,
    /// Une clé de signature autorisée.
    Delegate(Delegation),
}

/// Déclaration signée : « le bloc `block`, à la hauteur `height`, fait partie
/// de l'historique définitif ».
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub height: u64,
    pub block: Hash256,
    pub authority: Authority,
    pub signature: Signature,
}

impl Checkpoint {
    fn digest(genesis: &Hash256, height: u64, block: &Hash256) -> [u8; 32] {
        digest(tags::CHECKPOINT, genesis, &[&height.to_le_bytes(), block.as_bytes()])
    }

    /// Point signé directement par la clé maîtresse.
    pub fn sign_master(master: &KeyPair, genesis: &Hash256, height: u64, block: Hash256) -> Self {
        let signature = master.sign(&Self::digest(genesis, height, &block));
        Checkpoint { height, block, authority: Authority::Master, signature }
    }

    /// Point signé par une clé de signature déléguée.
    pub fn sign_delegate(
        key: &KeyPair,
        delegation: Delegation,
        genesis: &Hash256,
        height: u64,
        block: Hash256,
    ) -> Self {
        let signature = key.sign(&Self::digest(genesis, height, &block));
        Checkpoint { height, block, authority: Authority::Delegate(delegation), signature }
    }

    pub fn is_master(&self) -> bool {
        matches!(self.authority, Authority::Master)
    }

    /// Clé de signature déléguée (aucune pour un point de la clé maîtresse).
    pub fn delegate(&self) -> Option<PublicKey> {
        match &self.authority {
            Authority::Master => None,
            Authority::Delegate(d) => Some(d.signer),
        }
    }

    /// Vérifie la signature, la délégation, la révocation et la fin des
    /// points de contrôle.
    pub fn verify(
        &self,
        params: &ConsensusParams,
        genesis: &Hash256,
        revoked: &HashSet<PublicKey>,
    ) -> Result<(), CheckpointError> {
        let master = master_key(params)?;
        if self.height > params.checkpoint_sunset_height {
            return Err(CheckpointError::AfterSunset { height: self.height, sunset: params.checkpoint_sunset_height });
        }
        let key = match &self.authority {
            Authority::Master => master,
            Authority::Delegate(d) => {
                if !d.verify(&master, genesis) {
                    return Err(CheckpointError::BadDelegation);
                }
                if self.height > d.max_height {
                    return Err(CheckpointError::DelegationExpired { height: self.height, max_height: d.max_height });
                }
                if revoked.contains(&d.signer) {
                    return Err(CheckpointError::Revoked(d.signer));
                }
                d.signer
            }
        };
        if verify_signature(&key, &Self::digest(genesis, self.height, &self.block), &self.signature) {
            Ok(())
        } else {
            Err(CheckpointError::BadSignature)
        }
    }
}

/// Révocation, par la clé maîtresse, d'une clé de signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Revocation {
    pub signer: PublicKey,
    pub signature: Signature,
}

impl Revocation {
    fn digest(genesis: &Hash256, signer: &PublicKey) -> [u8; 32] {
        digest(tags::REVOCATION, genesis, &[&signer.0])
    }

    pub fn new(master: &KeyPair, genesis: &Hash256, signer: PublicKey) -> Self {
        Revocation { signer, signature: master.sign(&Self::digest(genesis, &signer)) }
    }

    pub fn verify(&self, params: &ConsensusParams, genesis: &Hash256) -> Result<(), CheckpointError> {
        let master = master_key(params)?;
        if verify_signature(&master, &Self::digest(genesis, &self.signer), &self.signature) {
            Ok(())
        } else {
            Err(CheckpointError::BadSignature)
        }
    }
}

fn read_key(r: &mut Reader<'_>) -> Result<PublicKey, DecodeError> {
    Ok(PublicKey(r.read_array::<PUBLIC_KEY_SIZE>()?))
}

fn read_signature(r: &mut Reader<'_>) -> Result<Signature, DecodeError> {
    Ok(Signature(r.read_array::<SIGNATURE_SIZE>()?))
}

impl Encode for Delegation {
    fn encode_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.signer.0);
        put_u64(out, self.max_height);
        out.extend_from_slice(&self.signature.0);
    }
}

impl Decode for Delegation {
    fn decode_from(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Delegation { signer: read_key(r)?, max_height: r.read_u64()?, signature: read_signature(r)? })
    }
}

impl Encode for Checkpoint {
    fn encode_to(&self, out: &mut Vec<u8>) {
        put_u64(out, self.height);
        out.extend_from_slice(self.block.as_bytes());
        match &self.authority {
            Authority::Master => put_u8(out, 0),
            Authority::Delegate(d) => {
                put_u8(out, 1);
                d.encode_to(out);
            }
        }
        out.extend_from_slice(&self.signature.0);
    }
}

impl Decode for Checkpoint {
    fn decode_from(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let height = r.read_u64()?;
        let block = Hash256(r.read_array::<32>()?);
        let authority = match r.read_u8()? {
            0 => Authority::Master,
            1 => Authority::Delegate(Delegation::decode_from(r)?),
            _ => return Err(DecodeError::InvalidValue("autorité de point de contrôle")),
        };
        Ok(Checkpoint { height, block, authority, signature: read_signature(r)? })
    }
}

impl Encode for Revocation {
    fn encode_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.signer.0);
        out.extend_from_slice(&self.signature.0);
    }
}

impl Decode for Revocation {
    fn decode_from(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Revocation { signer: read_key(r)?, signature: read_signature(r)? })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{REGTEST, REGTEST_CHECKPOINT_SECRET, TESTNET};

    fn master() -> KeyPair {
        KeyPair::from_secret_bytes(&REGTEST_CHECKPOINT_SECRET)
    }

    fn genesis() -> Hash256 {
        REGTEST.genesis.hash.unwrap()
    }

    fn block(n: u8) -> Hash256 {
        Hash256([n; 32])
    }

    #[test]
    fn regtest_master_key_matches_its_published_secret() {
        assert_eq!(Some(master().public_key()), REGTEST.checkpoint_master);
    }

    #[test]
    fn master_and_delegated_checkpoints_verify() {
        let none = HashSet::new();
        let cp = Checkpoint::sign_master(&master(), &genesis(), 10, block(1));
        assert_eq!(cp.verify(&REGTEST, &genesis(), &none), Ok(()));

        let hot = KeyPair::from_secret_bytes(&[5; 32]);
        let d = Delegation::new(&master(), &genesis(), hot.public_key(), 100);
        let cp = Checkpoint::sign_delegate(&hot, d, &genesis(), 100, block(2));
        assert_eq!(cp.verify(&REGTEST, &genesis(), &none), Ok(()));
        assert_eq!(cp.delegate(), Some(hot.public_key()));

        // Au-delà de la délégation.
        let late = Checkpoint::sign_delegate(&hot, d, &genesis(), 101, block(2));
        assert!(matches!(late.verify(&REGTEST, &genesis(), &none), Err(CheckpointError::DelegationExpired { .. })));

        // Clé révoquée.
        let revoked: HashSet<PublicKey> = [hot.public_key()].into();
        assert_eq!(cp.verify(&REGTEST, &genesis(), &revoked), Err(CheckpointError::Revoked(hot.public_key())));
    }

    #[test]
    fn forged_or_replayed_checkpoints_are_refused() {
        let none = HashSet::new();
        let hot = KeyPair::from_secret_bytes(&[5; 32]);
        let thief = KeyPair::from_secret_bytes(&[6; 32]);
        // Délégation que la clé maîtresse n'a pas signée.
        let fake = Delegation::new(&thief, &genesis(), thief.public_key(), 100);
        let cp = Checkpoint::sign_delegate(&thief, fake, &genesis(), 5, block(1));
        assert_eq!(cp.verify(&REGTEST, &genesis(), &none), Err(CheckpointError::BadDelegation));

        // Bloc ou hauteur modifiés après signature.
        let d = Delegation::new(&master(), &genesis(), hot.public_key(), 100);
        let mut cp = Checkpoint::sign_delegate(&hot, d, &genesis(), 5, block(1));
        cp.block = block(9);
        assert_eq!(cp.verify(&REGTEST, &genesis(), &none), Err(CheckpointError::BadSignature));
        let mut cp = Checkpoint::sign_delegate(&hot, d, &genesis(), 5, block(1));
        cp.height = 6;
        assert_eq!(cp.verify(&REGTEST, &genesis(), &none), Err(CheckpointError::BadSignature));

        // Délégation dont la durée a été allongée.
        let mut longer = d;
        longer.max_height = 1_000_000;
        let cp = Checkpoint::sign_delegate(&hot, longer, &genesis(), 500, block(1));
        assert_eq!(cp.verify(&REGTEST, &genesis(), &none), Err(CheckpointError::BadDelegation));

        // Un point d'un autre réseau (autre genèse) ne vaut rien ici.
        let other = Hash256([1; 32]);
        let cp = Checkpoint::sign_master(&master(), &other, 5, block(1));
        assert_eq!(cp.verify(&REGTEST, &genesis(), &none), Err(CheckpointError::BadSignature));
    }

    #[test]
    fn revocations_need_the_master_key() {
        let hot = KeyPair::from_secret_bytes(&[5; 32]);
        let r = Revocation::new(&master(), &genesis(), hot.public_key());
        assert_eq!(r.verify(&REGTEST, &genesis()), Ok(()));
        let forged = Revocation::new(&hot, &genesis(), hot.public_key());
        assert_eq!(forged.verify(&REGTEST, &genesis()), Err(CheckpointError::BadSignature));
    }

    #[test]
    fn sunset_and_disabled_networks() {
        let none = HashSet::new();
        let mut params = REGTEST.clone();
        params.checkpoint_sunset_height = 50;
        let ok = Checkpoint::sign_master(&master(), &genesis(), 50, block(1));
        assert_eq!(ok.verify(&params, &genesis(), &none), Ok(()));
        let late = Checkpoint::sign_master(&master(), &genesis(), 51, block(1));
        assert!(matches!(late.verify(&params, &genesis(), &none), Err(CheckpointError::AfterSunset { .. })));

        let mut disabled = TESTNET.clone();
        disabled.checkpoint_master = None;
        assert_eq!(ok.verify(&disabled, &genesis(), &none), Err(CheckpointError::Disabled));
    }

    #[test]
    fn encoding_roundtrip_and_rejects_garbage() {
        let hot = KeyPair::from_secret_bytes(&[5; 32]);
        let d = Delegation::new(&master(), &genesis(), hot.public_key(), 100);
        for cp in [
            Checkpoint::sign_master(&master(), &genesis(), 7, block(3)),
            Checkpoint::sign_delegate(&hot, d, &genesis(), 8, block(4)),
        ] {
            assert_eq!(Checkpoint::from_bytes(&cp.to_bytes()).unwrap(), cp);
        }
        let r = Revocation::new(&master(), &genesis(), hot.public_key());
        assert_eq!(Revocation::from_bytes(&r.to_bytes()).unwrap(), r);

        let mut bad = Checkpoint::sign_master(&master(), &genesis(), 7, block(3)).to_bytes();
        bad[40] = 2; // autorité inconnue
        assert!(Checkpoint::from_bytes(&bad).is_err());
        assert!(Checkpoint::from_bytes(&[0; 10]).is_err());
    }
}
