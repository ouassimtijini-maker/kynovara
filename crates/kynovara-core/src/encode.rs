//! Encodage binaire strict et canonique.
//!
//! Règles (consensus) :
//! - entiers à taille fixe en petit-boutiste ;
//! - listes précédées de leur longueur sur 4 octets, toujours bornée ;
//! - un message doit être lu **entièrement** : tout octet en trop est une
//!   erreur.
//!
//! Il n'existe donc qu'une seule façon d'encoder une valeur donnée, ce qui
//! empêche toute « malléabilité » de l'encodage.

use std::fmt;

/// Erreur de décodage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// Les données se terminent trop tôt.
    UnexpectedEnd,
    /// Il reste des octets non lus.
    TrailingBytes(usize),
    /// Une longueur dépasse le maximum autorisé.
    LengthTooLarge { len: u64, max: u64 },
    /// Valeur interdite (étiquette inconnue, texte invalide…).
    InvalidValue(&'static str),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::UnexpectedEnd => write!(f, "données tronquées"),
            DecodeError::TrailingBytes(n) => write!(f, "{n} octet(s) en trop"),
            DecodeError::LengthTooLarge { len, max } => {
                write!(f, "longueur {len} supérieure au maximum autorisé ({max})")
            }
            DecodeError::InvalidValue(what) => write!(f, "valeur invalide : {what}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Lecteur séquentiel sur une tranche d'octets.
#[derive(Debug)]
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    /// Nombre d'octets restant à lire.
    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub fn read_bytes(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.remaining() < n {
            return Err(DecodeError::UnexpectedEnd);
        }
        let out = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    pub fn read_array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.read_bytes(N)?);
        Ok(out)
    }

    pub fn read_u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.read_array::<1>()?[0])
    }

    pub fn read_u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_le_bytes(self.read_array()?))
    }

    pub fn read_u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.read_array()?))
    }

    pub fn read_u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_le_bytes(self.read_array()?))
    }

    /// Lit une longueur de liste (4 octets) en vérifiant :
    /// - qu'elle ne dépasse pas `max` ;
    /// - qu'il reste au moins `len × min_item_size` octets.
    ///
    /// La seconde vérification empêche un pair malveillant de nous faire
    /// réserver une énorme quantité de mémoire avec quelques octets.
    pub fn read_len(&mut self, max: usize, min_item_size: usize) -> Result<usize, DecodeError> {
        let len = self.read_u32()? as usize;
        if len > max {
            return Err(DecodeError::LengthTooLarge { len: len as u64, max: max as u64 });
        }
        if len.saturating_mul(min_item_size) > self.remaining() {
            return Err(DecodeError::UnexpectedEnd);
        }
        Ok(len)
    }

    /// Vérifie que tout a été lu.
    pub fn finish(self) -> Result<(), DecodeError> {
        match self.remaining() {
            0 => Ok(()),
            n => Err(DecodeError::TrailingBytes(n)),
        }
    }
}

/// Type encodable.
pub trait Encode {
    fn encode_to(&self, out: &mut Vec<u8>);

    fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_to(&mut out);
        out
    }
}

/// Type décodable.
pub trait Decode: Sized {
    fn decode_from(r: &mut Reader<'_>) -> Result<Self, DecodeError>;

    /// Décode une valeur qui doit occuper exactement `bytes`.
    fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader::new(bytes);
        let v = Self::decode_from(&mut r)?;
        r.finish()?;
        Ok(v)
    }
}

pub fn put_u8(out: &mut Vec<u8>, v: u8) {
    out.push(v);
}

pub fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Écrit une longueur de liste. Les longueurs sont toujours bornées bien
/// en dessous de 2^32 par les règles de validation.
pub fn put_len(out: &mut Vec<u8>, len: usize) {
    put_u32(out, u32::try_from(len).unwrap_or(u32::MAX));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_little_endian() {
        let mut out = Vec::new();
        put_u16(&mut out, 0x0102);
        put_u32(&mut out, 0x0304_0506);
        put_u64(&mut out, 7);
        assert_eq!(out, [2, 1, 6, 5, 4, 3, 7, 0, 0, 0, 0, 0, 0, 0]);
        let mut r = Reader::new(&out);
        assert_eq!(r.read_u16().unwrap(), 0x0102);
        assert_eq!(r.read_u32().unwrap(), 0x0304_0506);
        assert_eq!(r.read_u64().unwrap(), 7);
        r.finish().unwrap();
    }

    #[test]
    fn truncation_and_trailing() {
        let mut r = Reader::new(&[1, 2, 3]);
        assert_eq!(r.read_u32(), Err(DecodeError::UnexpectedEnd));
        let mut r = Reader::new(&[1, 2, 3]);
        r.read_u8().unwrap();
        assert_eq!(r.finish(), Err(DecodeError::TrailingBytes(2)));
    }

    #[test]
    fn length_guards() {
        // Longueur annoncée 1 000 000 > max.
        let mut buf = Vec::new();
        put_u32(&mut buf, 1_000_000);
        let mut r = Reader::new(&buf);
        assert!(matches!(r.read_len(10, 1), Err(DecodeError::LengthTooLarge { .. })));
        // Longueur 5 autorisée, mais pas assez d'octets derrière.
        let mut buf = Vec::new();
        put_u32(&mut buf, 5);
        buf.extend_from_slice(&[0; 9]);
        let mut r = Reader::new(&buf);
        assert_eq!(r.read_len(10, 2), Err(DecodeError::UnexpectedEnd));
        let mut r = Reader::new(&buf);
        assert_eq!(r.read_len(10, 1).unwrap(), 5);
    }
}
