//! Adresses Kynovara.
//!
//! Une adresse est l'encodage Bech32m de : `version (0)` + empreinte de clé
//! publique (20 octets). Le préfixe dépend du réseau (`kyn` pour le réseau
//! principal, `tkyn` pour le réseau de test, `rkyn` pour le réseau local de
//! développement). Une adresse d'un réseau est refusée sur un autre.

use crate::bech32::{self, Bech32Error};
use crate::crypto::{PUBKEY_HASH_SIZE, PubKeyHash, PublicKey};
use crate::params::Network;
use std::fmt;

/// Version actuelle du format d'adresse.
pub const ADDRESS_VERSION: u8 = 0;

/// Adresse liée à un réseau.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Address {
    network: Network,
    hash: PubKeyHash,
}

/// Erreur de lecture d'adresse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddressError {
    Bech32(Bech32Error),
    WrongNetwork { expected: Network, prefix: String },
    UnknownPrefix(String),
    UnknownVersion(u8),
    BadLength(usize),
    Empty,
}

impl fmt::Display for AddressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AddressError::Bech32(e) => write!(f, "adresse invalide : {e}"),
            AddressError::WrongNetwork { expected, prefix } => write!(
                f,
                "cette adresse (préfixe « {prefix} ») n'appartient pas au réseau {} (préfixe attendu « {} »)",
                expected.name(),
                expected.params().hrp
            ),
            AddressError::UnknownPrefix(p) => write!(f, "préfixe d'adresse inconnu : « {p} »"),
            AddressError::UnknownVersion(v) => write!(f, "version d'adresse inconnue : {v}"),
            AddressError::BadLength(n) => write!(f, "longueur d'adresse invalide ({n} octets)"),
            AddressError::Empty => write!(f, "adresse vide"),
        }
    }
}

impl std::error::Error for AddressError {}

impl From<Bech32Error> for AddressError {
    fn from(e: Bech32Error) -> Self {
        AddressError::Bech32(e)
    }
}

impl Address {
    pub fn new(network: Network, hash: PubKeyHash) -> Self {
        Address { network, hash }
    }

    pub fn from_public_key(network: Network, key: &PublicKey) -> Self {
        Address::new(network, key.pubkey_hash())
    }

    pub fn network(&self) -> Network {
        self.network
    }

    pub fn pubkey_hash(&self) -> PubKeyHash {
        self.hash
    }

    /// Texte de l'adresse.
    pub fn encode(&self) -> String {
        let mut data = vec![ADDRESS_VERSION];
        // 20 octets se convertissent toujours en 32 valeurs de 5 bits : aucune
        // erreur possible ici.
        data.extend(bech32::convert_bits(&self.hash.0, 8, 5, true).unwrap_or_default());
        bech32::encode(self.network.params().hrp, &data).unwrap_or_default()
    }

    /// Lit une adresse et vérifie qu'elle appartient au réseau `network`.
    pub fn parse(s: &str, network: Network) -> Result<Address, AddressError> {
        let addr = Address::parse_any(s)?;
        if addr.network != network {
            return Err(AddressError::WrongNetwork {
                expected: network,
                prefix: addr.network.params().hrp.to_string(),
            });
        }
        Ok(addr)
    }

    /// Lit une adresse de n'importe quel réseau connu.
    pub fn parse_any(s: &str) -> Result<Address, AddressError> {
        if s.is_empty() {
            return Err(AddressError::Empty);
        }
        let (hrp, data) = bech32::decode(s)?;
        let network = Network::ALL
            .into_iter()
            .find(|n| n.params().hrp == hrp)
            .ok_or_else(|| AddressError::UnknownPrefix(hrp.clone()))?;
        let (&version, rest) = data.split_first().ok_or(AddressError::Empty)?;
        if version != ADDRESS_VERSION {
            return Err(AddressError::UnknownVersion(version));
        }
        let bytes = bech32::convert_bits(rest, 5, 8, false)?;
        if bytes.len() != PUBKEY_HASH_SIZE {
            return Err(AddressError::BadLength(bytes.len()));
        }
        let mut hash = [0u8; PUBKEY_HASH_SIZE];
        hash.copy_from_slice(&bytes);
        Ok(Address { network, hash: PubKeyHash(hash) })
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.encode())
    }
}

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Address({})", self.encode())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::KeyPair;

    #[test]
    fn roundtrip_all_networks() {
        let key = KeyPair::from_secret_bytes(&[1u8; 32]).public_key();
        for net in Network::ALL {
            let a = Address::from_public_key(net, &key);
            let s = a.encode();
            assert!(s.starts_with(&format!("{}1", net.params().hrp)), "{s}");
            assert_eq!(Address::parse(&s, net).unwrap(), a);
            assert_eq!(Address::parse(&s.to_ascii_uppercase(), net).unwrap(), a);
        }
    }

    #[test]
    fn wrong_network_rejected() {
        let key = KeyPair::from_secret_bytes(&[2u8; 32]).public_key();
        let a = Address::from_public_key(Network::Testnet, &key).encode();
        assert!(matches!(Address::parse(&a, Network::Mainnet), Err(AddressError::WrongNetwork { .. })));
    }

    #[test]
    fn typo_rejected() {
        let key = KeyPair::from_secret_bytes(&[3u8; 32]).public_key();
        let a = Address::from_public_key(Network::Regtest, &key).encode();
        // Change un caractère de données.
        let mut chars: Vec<char> = a.chars().collect();
        let i = chars.len() - 10;
        chars[i] = if chars[i] == 'q' { 'p' } else { 'q' };
        let typo: String = chars.into_iter().collect();
        assert!(matches!(
            Address::parse(&typo, Network::Regtest),
            Err(AddressError::Bech32(Bech32Error::InvalidChecksum))
        ));
        assert_eq!(Address::parse("", Network::Regtest), Err(AddressError::Empty));
    }

    #[test]
    fn wrong_version_and_length() {
        // Version 1.
        let mut data = vec![1u8];
        data.extend(bech32::convert_bits(&[0u8; 20], 8, 5, true).unwrap());
        let s = bech32::encode("rkyn", &data).unwrap();
        assert_eq!(Address::parse_any(&s), Err(AddressError::UnknownVersion(1)));
        // Empreinte de 19 octets.
        let mut data = vec![0u8];
        data.extend(bech32::convert_bits(&[0u8; 19], 8, 5, true).unwrap());
        let s = bech32::encode("rkyn", &data).unwrap();
        assert!(Address::parse_any(&s).is_err());
        // Préfixe inconnu.
        let mut data = vec![0u8];
        data.extend(bech32::convert_bits(&[0u8; 20], 8, 5, true).unwrap());
        let s = bech32::encode("bc", &data).unwrap();
        assert_eq!(Address::parse_any(&s), Err(AddressError::UnknownPrefix("bc".into())));
    }
}
