//! Encodage hexadécimal (sortie en minuscules, entrée insensible à la casse).

use std::fmt;

/// Erreur de décodage hexadécimal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HexError {
    /// Nombre impair de caractères.
    OddLength,
    /// Caractère non hexadécimal à la position indiquée.
    InvalidChar(usize),
    /// Longueur différente de celle attendue (en caractères).
    WrongLength { expected: usize, actual: usize },
}

impl fmt::Display for HexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HexError::OddLength => write!(f, "nombre impair de caractères hexadécimaux"),
            HexError::InvalidChar(i) => write!(f, "caractère hexadécimal invalide à la position {i}"),
            HexError::WrongLength { expected, actual } => {
                write!(f, "longueur hexadécimale invalide : {actual} caractères au lieu de {expected}")
            }
        }
    }
}

impl std::error::Error for HexError {}

const DIGITS: &[u8; 16] = b"0123456789abcdef";

/// Encode des octets en texte hexadécimal minuscule.
pub fn encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(char::from(DIGITS[usize::from(b >> 4)]));
        s.push(char::from(DIGITS[usize::from(b & 0x0f)]));
    }
    s
}

fn nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Décode un texte hexadécimal.
pub fn decode(s: &str) -> Result<Vec<u8>, HexError> {
    let bytes = s.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err(HexError::OddLength);
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for (i, pair) in bytes.chunks_exact(2).enumerate() {
        let hi = nibble(pair[0]).ok_or(HexError::InvalidChar(i * 2))?;
        let lo = nibble(pair[1]).ok_or(HexError::InvalidChar(i * 2 + 1))?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

/// Décode un texte hexadécimal de taille fixe (N octets).
pub fn decode_array<const N: usize>(s: &str) -> Result<[u8; N], HexError> {
    if s.len() != N * 2 {
        return Err(HexError::WrongLength { expected: N * 2, actual: s.len() });
    }
    let v = decode(s)?;
    let mut out = [0u8; N];
    out.copy_from_slice(&v);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let data = [0u8, 1, 0x7f, 0x80, 0xab, 0xff];
        let s = encode(&data);
        assert_eq!(s, "00017f80abff");
        assert_eq!(decode(&s).unwrap(), data);
        assert_eq!(decode("00017F80ABFF").unwrap(), data);
    }

    #[test]
    fn errors() {
        assert_eq!(decode("abc"), Err(HexError::OddLength));
        assert_eq!(decode("zz"), Err(HexError::InvalidChar(0)));
        assert_eq!(decode("0g"), Err(HexError::InvalidChar(1)));
        assert_eq!(decode_array::<2>("00"), Err(HexError::WrongLength { expected: 4, actual: 2 }));
        assert_eq!(decode_array::<2>("0102").unwrap(), [1, 2]);
    }
}
