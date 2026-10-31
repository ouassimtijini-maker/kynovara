//! Blocs et en-têtes.
//!
//! En-tête (116 octets, règle de consensus) :
//!
//! | champ         | taille | encodage                    |
//! |---------------|--------|-----------------------------|
//! | version       | 4      | petit-boutiste              |
//! | bloc parent   | 32     | octets bruts                |
//! | racine Merkle | 32     | octets bruts                |
//! | horodatage    | 8      | petit-boutiste (secondes)   |
//! | cible         | 32     | gros-boutiste (lisible)     |
//! | nonce         | 8      | petit-boutiste              |
//!
//! L'empreinte du bloc est le double SHA-256 de l'en-tête. La preuve de
//! travail est valide si cette empreinte, lue comme un nombre de 256 bits en
//! gros-boutiste, est inférieure ou égale à la cible.

use crate::encode::{Decode, DecodeError, Encode, Reader, put_len};
use crate::hash::{Hash256, sha256d};
use crate::merkle::merkle_root;
use crate::tx::Transaction;
use crate::u256::U256;

/// Version d'en-tête acceptée (règle de consensus).
pub const BLOCK_VERSION: u32 = 1;
/// Taille d'un en-tête encodé.
pub const HEADER_SIZE: usize = 116;
/// Taille maximale d'un bloc encodé (règle de consensus).
pub const MAX_BLOCK_SIZE: usize = 1_000_000;
/// Plus petite transaction possible (récompense sans sortie) : borne de décodage.
const MIN_TX_SIZE: usize = 4 + 1 + 8 + 4 + 4;
/// Nombre maximal de transactions décodables dans un bloc.
pub const MAX_BLOCK_TXS: usize = MAX_BLOCK_SIZE / MIN_TX_SIZE;

/// En-tête de bloc.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BlockHeader {
    pub version: u32,
    pub prev_hash: Hash256,
    pub merkle_root: Hash256,
    pub timestamp: u64,
    pub target: U256,
    pub nonce: u64,
}

impl BlockHeader {
    /// Les 116 octets de l'en-tête.
    pub fn to_array(&self) -> [u8; HEADER_SIZE] {
        let mut out = [0u8; HEADER_SIZE];
        out[0..4].copy_from_slice(&self.version.to_le_bytes());
        out[4..36].copy_from_slice(&self.prev_hash.0);
        out[36..68].copy_from_slice(&self.merkle_root.0);
        out[68..76].copy_from_slice(&self.timestamp.to_le_bytes());
        out[76..108].copy_from_slice(&self.target.to_be_bytes());
        out[108..116].copy_from_slice(&self.nonce.to_le_bytes());
        out
    }

    /// Empreinte du bloc (double SHA-256 de l'en-tête).
    pub fn hash(&self) -> Hash256 {
        sha256d(&self.to_array())
    }

    /// La preuve de travail est-elle satisfaite (empreinte ≤ cible) ?
    pub fn meets_target(&self) -> bool {
        hash_meets_target(&self.hash(), &self.target)
    }
}

/// Comparaison empreinte ≤ cible (les deux en gros-boutiste).
pub fn hash_meets_target(hash: &Hash256, target: &U256) -> bool {
    hash.0 <= target.to_be_bytes()
}

impl Encode for BlockHeader {
    fn encode_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.to_array());
    }
}

impl Decode for BlockHeader {
    fn decode_from(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(BlockHeader {
            version: r.read_u32()?,
            prev_hash: Hash256(r.read_array()?),
            merkle_root: Hash256(r.read_array()?),
            timestamp: r.read_u64()?,
            target: U256::from_be_bytes(r.read_array()?),
            nonce: r.read_u64()?,
        })
    }
}

/// Bloc : en-tête + transactions (la première est la récompense).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<Transaction>,
}

impl Block {
    pub fn hash(&self) -> Hash256 {
        self.header.hash()
    }

    /// Racine de Merkle calculée à partir des transactions (sur les wtxid).
    pub fn compute_merkle_root(&self) -> Hash256 {
        let ids: Vec<Hash256> = self.transactions.iter().map(Transaction::wtxid).collect();
        merkle_root(&ids)
    }

    /// Taille encodée, en octets.
    pub fn size(&self) -> usize {
        HEADER_SIZE + 4 + self.transactions.iter().map(Transaction::size).sum::<usize>()
    }

    /// Transaction de récompense (si le bloc est bien formé).
    pub fn coinbase(&self) -> Option<&Transaction> {
        self.transactions.first().filter(|t| t.is_coinbase())
    }
}

impl Encode for Block {
    fn encode_to(&self, out: &mut Vec<u8>) {
        out.reserve(self.size());
        self.header.encode_to(out);
        put_len(out, self.transactions.len());
        for tx in &self.transactions {
            tx.encode_to(out);
        }
    }
}

impl Decode for Block {
    fn decode_from(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let header = BlockHeader::decode_from(r)?;
        let count = r.read_len(MAX_BLOCK_TXS, MIN_TX_SIZE)?;
        let mut transactions = Vec::with_capacity(count);
        for _ in 0..count {
            transactions.push(Transaction::decode_from(r)?);
        }
        Ok(Block { header, transactions })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::PubKeyHash;
    use crate::tx::TxOut;

    fn sample() -> Block {
        let cb = Transaction::coinbase(1, vec![], vec![TxOut { value: 50, recipient: PubKeyHash([1; 20]) }]);
        let mut block = Block {
            header: BlockHeader {
                version: BLOCK_VERSION,
                prev_hash: Hash256([3; 32]),
                merkle_root: Hash256::ZERO,
                timestamp: 1_700_000_000,
                target: U256::low_bits(255),
                nonce: 42,
            },
            transactions: vec![cb],
        };
        block.header.merkle_root = block.compute_merkle_root();
        block
    }

    #[test]
    fn header_layout() {
        let b = sample();
        let bytes = b.header.to_array();
        assert_eq!(&bytes[0..4], &[1, 0, 0, 0]);
        assert_eq!(&bytes[4..36], &[3; 32]);
        assert_eq!(bytes[76], 0x7f);
        assert_eq!(&bytes[108..116], &42u64.to_le_bytes());
        assert_eq!(BlockHeader::from_bytes(&bytes).unwrap(), b.header);
    }

    #[test]
    fn block_roundtrip_and_size() {
        let b = sample();
        let bytes = b.to_bytes();
        assert_eq!(bytes.len(), b.size());
        assert_eq!(Block::from_bytes(&bytes).unwrap(), b);
        let mut extra = bytes.clone();
        extra.push(1);
        assert!(Block::from_bytes(&extra).is_err());
        for cut in [0, 50, HEADER_SIZE, bytes.len() - 1] {
            assert!(Block::from_bytes(&bytes[..cut]).is_err());
        }
    }

    #[test]
    fn target_comparison_is_numeric() {
        let mut h = [0u8; 32];
        h[0] = 0x7f;
        assert!(hash_meets_target(&Hash256(h), &U256::low_bits(255)));
        h[0] = 0x80;
        assert!(!hash_meets_target(&Hash256(h), &U256::low_bits(255)));
        assert!(hash_meets_target(&Hash256::ZERO, &U256::ZERO));
    }

    #[test]
    fn huge_tx_count_is_rejected_cheaply() {
        let mut bytes = sample().header.to_array().to_vec();
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(Block::from_bytes(&bytes).is_err());
    }
}
