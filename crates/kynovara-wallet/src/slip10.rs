//! Dérivation hiérarchique des clés Ed25519 (SLIP-0010).
//!
//! Chemin utilisé par Kynovara : `m / 44' / type' / 0' / branche' / index'`
//! (toutes les étapes sont « durcies », seule possibilité pour Ed25519).
//! `type` est le type de pièce SLIP-44 du réseau ; `branche` vaut 0 pour les
//! adresses de réception et 1 pour la monnaie rendue.

use hmac::{Hmac, Mac};
use sha2::Sha512;
use zeroize::Zeroizing;

type HmacSha512 = Hmac<Sha512>;

/// Bit des index « durcis ».
pub const HARDENED: u32 = 0x8000_0000;

/// Clé étendue (clé privée + code de chaîne), effacée de la mémoire à la fin.
pub struct ExtendedKey {
    pub key: Zeroizing<[u8; 32]>,
    pub chain_code: Zeroizing<[u8; 32]>,
}

fn hmac_sha512(key: &[u8], parts: &[&[u8]]) -> Zeroizing<[u8; 64]> {
    // HMAC accepte des clés de toute longueur : l'erreur est impossible.
    let mut mac = match <HmacSha512 as Mac>::new_from_slice(key) {
        Ok(mac) => mac,
        Err(_) => unreachable!("HMAC accepte toute longueur de clé"),
    };
    for p in parts {
        mac.update(p);
    }
    let mut out = Zeroizing::new([0u8; 64]);
    out.copy_from_slice(&mac.finalize().into_bytes());
    out
}

fn split(i: &[u8; 64]) -> ExtendedKey {
    let mut key = Zeroizing::new([0u8; 32]);
    let mut chain_code = Zeroizing::new([0u8; 32]);
    key.copy_from_slice(&i[..32]);
    chain_code.copy_from_slice(&i[32..]);
    ExtendedKey { key, chain_code }
}

/// Clé maîtresse à partir de la graine.
pub fn master(seed: &[u8]) -> ExtendedKey {
    split(&hmac_sha512(b"ed25519 seed", &[seed]))
}

impl ExtendedKey {
    /// Dérive l'enfant durci `index` (le bit de durcissement est ajouté).
    pub fn derive(&self, index: u32) -> ExtendedKey {
        let i = (index | HARDENED).to_be_bytes();
        split(&hmac_sha512(&self.chain_code[..], &[&[0u8], &self.key[..], &i]))
    }
}

/// Dérive un chemin complet (chaque index est durci).
pub fn derive_path(seed: &[u8], path: &[u32]) -> ExtendedKey {
    let mut k = master(seed);
    for index in path {
        k = k.derive(*index);
    }
    k
}

#[cfg(test)]
mod tests {
    use super::*;
    use kynovara_core::crypto::KeyPair;
    use kynovara_core::hex;

    #[test]
    fn slip10_test_vector_1() {
        // SLIP-0010, « Test vector 1 for ed25519 ».
        let seed = hex::decode("000102030405060708090a0b0c0d0e0f").unwrap();
        let cases: [(&[u32], &str, &str, &str); 6] = [
            (
                &[],
                "90046a93de5380a72b5e45010748567d5ea02bbf6522f979e05c0d8d8ca9fffb",
                "2b4be7f19ee27bbf30c667b642d5f4aa69fd169872f8fc3059c08ebae2eb19e7",
                "a4b2856bfec510abab89753fac1ac0e1112364e7d250545963f135f2a33188ed",
            ),
            (
                &[0],
                "8b59aa11380b624e81507a27fedda59fea6d0b779a778918a2fd3590e16e9c69",
                "68e0fe46dfb67e368c75379acec591dad19df3cde26e63b93a8e704f1dade7a3",
                "8c8a13df77a28f3445213a0f432fde644acaa215fc72dcdf300d5efaa85d350c",
            ),
            (
                &[0, 1],
                "a320425f77d1b5c2505a6b1b27382b37368ee640e3557c315416801243552f14",
                "b1d0bad404bf35da785a64ca1ac54b2617211d2777696fbffaf208f746ae84f2",
                "1932a5270f335bed617d5b935c80aedb1a35bd9fc1e31acafd5372c30f5c1187",
            ),
            (
                &[0, 1, 2],
                "2e69929e00b5ab250f49c3fb1c12f252de4fed2c1db88387094a0f8c4c9ccd6c",
                "92a5b23c0b8a99e37d07df3fb9966917f5d06e02ddbd909c7e184371463e9fc9",
                "ae98736566d30ed0e9d2f4486a64bc95740d89c7db33f52121f8ea8f76ff0fc1",
            ),
            (
                &[0, 1, 2, 2],
                "8f6d87f93d750e0efccda017d662a1b31a266e4a6f5993b15f5c1f07f74dd5cc",
                "30d1dc7e5fc04c31219ab25a27ae00b50f6fd66622f6e9c913253d6511d1e662",
                "8abae2d66361c879b900d204ad2cc4984fa2aa344dd7ddc46007329ac76c429c",
            ),
            (
                &[0, 1, 2, 2, 1_000_000_000],
                "68789923a0cac2cd5a29172a475fe9e0fb14cd6adb5ad98a3fa70333e7afa230",
                "8f94d394a8e8fd6b1bc2f3f49f5c47e385281d5c17e65324b0f62483e37e8793",
                "3c24da049451555d51a7014a37337aa4e12d41e485abccfa46b47dfb2af54b7a",
            ),
        ];
        for (path, chain, private, public) in cases {
            let k = derive_path(&seed, path);
            assert_eq!(hex::encode(&k.chain_code[..]), chain, "{path:?}");
            assert_eq!(hex::encode(&k.key[..]), private, "{path:?}");
            let kp = KeyPair::from_secret_bytes(&k.key);
            assert_eq!(kp.public_key().to_hex(), public, "{path:?}");
        }
    }
}
