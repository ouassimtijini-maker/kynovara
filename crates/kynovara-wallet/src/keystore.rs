//! Fichier de portefeuille chiffré.
//!
//! - La clé de chiffrement est dérivée du mot de passe par **Argon2id**
//!   (64 Mio de mémoire, 3 passes) : chaque essai de mot de passe coûte cher à
//!   un voleur du fichier.
//! - Le secret (l'entropie de la phrase de 24 mots) est chiffré par
//!   **XChaCha20-Poly1305** avec un nonce aléatoire de 24 octets.
//! - Les champs non secrets (réseau, paramètres, index d'adresses…) sont
//!   authentifiés comme « données associées » : toute modification du fichier
//!   est détectée au déverrouillage.
//! - L'écriture est atomique (fichier temporaire puis renommage) et, sous
//!   Linux/macOS, le fichier n'est lisible que par son propriétaire.

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use kynovara_core::hex;
use kynovara_core::params::Network;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use zeroize::Zeroizing;

const FORMAT: &str = "kynovara-wallet";
const VERSION: u32 = 1;
/// Paramètres Argon2id par défaut.
pub const DEFAULT_MEMORY_KIB: u32 = 64 * 1024;
pub const DEFAULT_ITERATIONS: u32 = 3;
pub const DEFAULT_PARALLELISM: u32 = 1;
/// Longueur minimale du mot de passe.
pub const MIN_PASSWORD_LEN: usize = 8;

/// Paramètres de dérivation de clé.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct KdfParams {
    pub algorithm: String,
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
    pub salt: String,
}

/// Paramètres de chiffrement.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CipherParams {
    pub algorithm: String,
    pub nonce: String,
}

/// Contenu du fichier `wallet.json`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WalletFile {
    pub format: String,
    pub version: u32,
    pub network: String,
    pub kdf: KdfParams,
    pub cipher: CipherParams,
    pub ciphertext: String,
    /// Prochaine adresse de réception à distribuer.
    pub receive_index: u32,
    /// Prochaine adresse de monnaie rendue.
    pub change_index: u32,
}

/// Erreur de fichier de portefeuille.
#[derive(Debug)]
pub enum KeystoreError {
    Io(String),
    Format(String),
    WrongPasswordOrTampered,
    WeakPassword,
    Crypto(String),
}

impl fmt::Display for KeystoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeystoreError::Io(e) => write!(f, "{e}"),
            KeystoreError::Format(e) => write!(f, "fichier de portefeuille invalide : {e}"),
            KeystoreError::WrongPasswordOrTampered => {
                write!(f, "mot de passe incorrect (ou fichier de portefeuille modifié)")
            }
            KeystoreError::WeakPassword => {
                write!(f, "mot de passe trop court ({MIN_PASSWORD_LEN} caractères minimum)")
            }
            KeystoreError::Crypto(e) => write!(f, "erreur cryptographique : {e}"),
        }
    }
}

impl std::error::Error for KeystoreError {}

fn random_bytes<const N: usize>() -> Result<[u8; N], KeystoreError> {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b)
        .map_err(|e| KeystoreError::Crypto(format!("générateur aléatoire indisponible : {e}")))?;
    Ok(b)
}

fn derive_key(password: &str, kdf: &KdfParams) -> Result<Zeroizing<[u8; 32]>, KeystoreError> {
    let salt = hex::decode(&kdf.salt).map_err(|e| KeystoreError::Format(format!("sel : {e}")))?;
    let params = Params::new(kdf.memory_kib, kdf.iterations, kdf.parallelism, Some(32))
        .map_err(|e| KeystoreError::Format(format!("paramètres Argon2 : {e}")))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = Zeroizing::new([0u8; 32]);
    argon
        .hash_password_into(password.as_bytes(), &salt, &mut key[..])
        .map_err(|e| KeystoreError::Crypto(format!("Argon2 : {e}")))?;
    Ok(key)
}

impl WalletFile {
    /// Données associées : tout ce qui n'est pas secret mais doit être intègre.
    fn associated_data(&self) -> Vec<u8> {
        format!(
            "{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
            self.format,
            self.version,
            self.network,
            self.kdf.algorithm,
            self.kdf.memory_kib,
            self.kdf.iterations,
            self.kdf.parallelism,
            self.kdf.salt,
            self.cipher.algorithm,
            self.receive_index,
            self.change_index,
        )
        .into_bytes()
    }

    /// Vérifie les champs avant toute opération coûteuse (un fichier piégé ne
    /// doit pas pouvoir exiger des giga-octets de mémoire).
    fn validate(&self) -> Result<(), KeystoreError> {
        let bad = |m: &str| Err(KeystoreError::Format(m.to_string()));
        if self.format != FORMAT {
            return bad("format inconnu");
        }
        if self.version != VERSION {
            return bad("version de fichier non prise en charge");
        }
        if Network::from_name(&self.network).is_none() {
            return bad("réseau inconnu");
        }
        if self.kdf.algorithm != "argon2id" || self.cipher.algorithm != "xchacha20-poly1305" {
            return bad("algorithme non pris en charge");
        }
        if !(8 * 1024..=4 * 1024 * 1024).contains(&self.kdf.memory_kib)
            || !(1..=100).contains(&self.kdf.iterations)
            || !(1..=16).contains(&self.kdf.parallelism)
        {
            return bad("paramètres Argon2 hors limites");
        }
        if self.kdf.salt.len() != 32 || self.cipher.nonce.len() != 48 || self.ciphertext.len() != 96 {
            return bad("longueur de sel, de nonce ou de données chiffrées incorrecte");
        }
        Ok(())
    }

    /// Chiffre une nouvelle entropie avec un mot de passe.
    pub fn create(network: Network, entropy: &[u8; 32], password: &str) -> Result<WalletFile, KeystoreError> {
        Self::create_with_params(network, entropy, password, DEFAULT_MEMORY_KIB, DEFAULT_ITERATIONS)
    }

    /// Comme [`create`](Self::create) avec des paramètres Argon2 explicites.
    pub fn create_with_params(
        network: Network,
        entropy: &[u8; 32],
        password: &str,
        memory_kib: u32,
        iterations: u32,
    ) -> Result<WalletFile, KeystoreError> {
        if password.chars().count() < MIN_PASSWORD_LEN {
            return Err(KeystoreError::WeakPassword);
        }
        let mut file = WalletFile {
            format: FORMAT.into(),
            version: VERSION,
            network: network.name().into(),
            kdf: KdfParams {
                algorithm: "argon2id".into(),
                memory_kib,
                iterations,
                parallelism: DEFAULT_PARALLELISM,
                salt: hex::encode(&random_bytes::<16>()?),
            },
            cipher: CipherParams { algorithm: "xchacha20-poly1305".into(), nonce: String::new() },
            ciphertext: String::new(),
            receive_index: 0,
            change_index: 0,
        };
        let key = derive_key(password, &file.kdf)?;
        file.seal(&key, entropy)?;
        Ok(file)
    }

    /// (Re)chiffre l'entropie avec un nouveau nonce et les données associées
    /// actuelles (à appeler après toute modification d'un champ).
    fn seal(&mut self, key: &[u8; 32], entropy: &[u8; 32]) -> Result<(), KeystoreError> {
        let nonce = random_bytes::<24>()?;
        self.cipher.nonce = hex::encode(&nonce);
        let aad = self.associated_data();
        let cipher = XChaCha20Poly1305::new(key.into());
        let ct = cipher
            .encrypt(XNonce::from_slice(&nonce), Payload { msg: entropy, aad: &aad })
            .map_err(|_| KeystoreError::Crypto("chiffrement impossible".into()))?;
        self.ciphertext = hex::encode(&ct);
        Ok(())
    }

    /// Déchiffre l'entropie.
    pub fn unlock(&self, password: &str) -> Result<UnlockedKey, KeystoreError> {
        self.validate()?;
        let key = derive_key(password, &self.kdf)?;
        let entropy = self.open_with_key(&key)?;
        Ok(UnlockedKey { key, entropy })
    }

    fn open_with_key(&self, key: &[u8; 32]) -> Result<Zeroizing<[u8; 32]>, KeystoreError> {
        let nonce = hex::decode(&self.cipher.nonce).map_err(|e| KeystoreError::Format(e.to_string()))?;
        let ct = hex::decode(&self.ciphertext).map_err(|e| KeystoreError::Format(e.to_string()))?;
        let aad = self.associated_data();
        let cipher = XChaCha20Poly1305::new(key.into());
        let plain = Zeroizing::new(
            cipher
                .decrypt(XNonce::from_slice(&nonce), Payload { msg: &ct, aad: &aad })
                .map_err(|_| KeystoreError::WrongPasswordOrTampered)?,
        );
        if plain.len() != 32 {
            return Err(KeystoreError::Format("contenu chiffré de taille inattendue".into()));
        }
        let mut entropy = Zeroizing::new([0u8; 32]);
        entropy.copy_from_slice(&plain);
        Ok(entropy)
    }

    /// Met à jour les index d'adresses (rechiffrement avec la clé déjà dérivée).
    pub fn update_indices(&mut self, unlocked: &UnlockedKey, receive: u32, change: u32) -> Result<(), KeystoreError> {
        self.receive_index = receive;
        self.change_index = change;
        self.seal(&unlocked.key, &unlocked.entropy)
    }

    /// Lit un fichier.
    pub fn load(path: &Path) -> Result<WalletFile, KeystoreError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| KeystoreError::Io(format!("lecture de {} impossible : {e}", path.display())))?;
        let file: WalletFile = serde_json::from_str(&text).map_err(|e| KeystoreError::Format(e.to_string()))?;
        file.validate()?;
        Ok(file)
    }

    /// Écrit le fichier de façon atomique.
    pub fn save(&self, path: &Path) -> Result<(), KeystoreError> {
        let io = |e: std::io::Error| KeystoreError::Io(format!("écriture de {} impossible : {e}", path.display()));
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| KeystoreError::Format(e.to_string()))?;
        let tmp = path.with_extension("json.tmp");
        let _ = std::fs::remove_file(&tmp);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut f = options.open(&tmp).map_err(io)?;
        f.write_all(text.as_bytes()).map_err(io)?;
        f.sync_all().map_err(io)?;
        drop(f);
        std::fs::rename(&tmp, path).map_err(io)
    }

    pub fn network(&self) -> Option<Network> {
        Network::from_name(&self.network)
    }
}

/// Clé dérivée et entropie déchiffrée (effacées de la mémoire à la fin).
pub struct UnlockedKey {
    key: Zeroizing<[u8; 32]>,
    pub entropy: Zeroizing<[u8; 32]>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Paramètres allégés pour des tests rapides (la mémoire minimale reste 8 Mio).
    fn create(entropy: &[u8; 32], password: &str) -> WalletFile {
        WalletFile::create_with_params(Network::Regtest, entropy, password, 8 * 1024, 1).unwrap()
    }

    #[test]
    fn roundtrip_and_wrong_password() {
        let entropy = [42u8; 32];
        let file = create(&entropy, "motdepasse-solide");
        let unlocked = file.unlock("motdepasse-solide").unwrap();
        assert_eq!(*unlocked.entropy, entropy);
        assert!(matches!(file.unlock("mauvais-mdp"), Err(KeystoreError::WrongPasswordOrTampered)));
        // Le texte chiffré ne contient pas l'entropie en clair.
        assert!(!file.ciphertext.contains(&hex::encode(&entropy)));
    }

    #[test]
    fn tampering_is_detected() {
        let file = create(&[1u8; 32], "motdepasse-solide");
        let mut t = file.clone();
        t.receive_index = 99;
        assert!(matches!(t.unlock("motdepasse-solide"), Err(KeystoreError::WrongPasswordOrTampered)));
        let mut t = file.clone();
        t.network = "testnet".into();
        assert!(matches!(t.unlock("motdepasse-solide"), Err(KeystoreError::WrongPasswordOrTampered)));
        let mut t = file.clone();
        t.kdf.memory_kib = 1 << 30;
        assert!(matches!(t.unlock("motdepasse-solide"), Err(KeystoreError::Format(_))));
        // Un bit du texte chiffré inversé (toujours une vraie modification ;
        // remplacer par « 00 » ne changeait rien une fois sur 256).
        let mut t = file;
        let mut ct = hex::decode(&t.ciphertext).unwrap();
        ct[0] ^= 0x01;
        t.ciphertext = hex::encode(&ct);
        assert!(matches!(t.unlock("motdepasse-solide"), Err(KeystoreError::WrongPasswordOrTampered)));
    }

    #[test]
    fn index_update_and_disk_roundtrip() {
        let dir = std::env::temp_dir().join(format!("kynovara-ks-{}", hex::encode(&random_bytes::<6>().unwrap())));
        let path = dir.join("wallet.json");
        let mut file = create(&[7u8; 32], "motdepasse-solide");
        let unlocked = file.unlock("motdepasse-solide").unwrap();
        file.update_indices(&unlocked, 5, 2).unwrap();
        file.save(&path).unwrap();
        let loaded = WalletFile::load(&path).unwrap();
        assert_eq!(loaded, file);
        assert_eq!(*loaded.unlock("motdepasse-solide").unwrap().entropy, [7u8; 32]);
        assert_eq!((loaded.receive_index, loaded.change_index), (5, 2));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn weak_password_refused() {
        assert!(matches!(WalletFile::create(Network::Regtest, &[0u8; 32], "court"), Err(KeystoreError::WeakPassword)));
    }
}
