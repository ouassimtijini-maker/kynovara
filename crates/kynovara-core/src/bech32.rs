//! Encodage Bech32m (BIP-350), utilisé pour les adresses.
//!
//! Bech32m détecte toutes les erreurs de frappe touchant jusqu'à 4
//! caractères : une adresse mal recopiée est refusée au lieu d'envoyer les
//! fonds dans le vide.

use std::fmt;

const CHARSET: &[u8; 32] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
const GENERATOR: [u32; 5] = [0x3b6a_57b2, 0x2650_8e6d, 0x1ea1_19fa, 0x3d42_33dd, 0x2a14_62b3];
const BECH32M_CONST: u32 = 0x2bc8_30a3;
/// Longueur maximale d'une chaîne Bech32m.
pub const MAX_LEN: usize = 90;
const CHECKSUM_LEN: usize = 6;

/// Erreur Bech32m.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bech32Error {
    TooLong,
    MixedCase,
    NoSeparator,
    EmptyHrp,
    InvalidHrpChar,
    InvalidDataChar,
    TooShortChecksum,
    InvalidChecksum,
    InvalidValue,
    InvalidPadding,
}

impl fmt::Display for Bech32Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            Bech32Error::TooLong => "chaîne trop longue",
            Bech32Error::MixedCase => "mélange de majuscules et de minuscules",
            Bech32Error::NoSeparator => "séparateur « 1 » absent",
            Bech32Error::EmptyHrp => "préfixe vide",
            Bech32Error::InvalidHrpChar => "caractère invalide dans le préfixe",
            Bech32Error::InvalidDataChar => "caractère invalide",
            Bech32Error::TooShortChecksum => "somme de contrôle trop courte",
            Bech32Error::InvalidChecksum => "somme de contrôle invalide (faute de frappe ?)",
            Bech32Error::InvalidValue => "valeur hors limites",
            Bech32Error::InvalidPadding => "remplissage invalide",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for Bech32Error {}

fn polymod(values: &[u8]) -> u32 {
    let mut chk: u32 = 1;
    for &v in values {
        let top = chk >> 25;
        chk = ((chk & 0x01ff_ffff) << 5) ^ u32::from(v);
        for (i, g) in GENERATOR.iter().enumerate() {
            if (top >> i) & 1 == 1 {
                chk ^= g;
            }
        }
    }
    chk
}

fn hrp_expand(hrp: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(hrp.len() * 2 + 1);
    out.extend(hrp.iter().map(|c| c >> 5));
    out.push(0);
    out.extend(hrp.iter().map(|c| c & 31));
    out
}

fn checksum(hrp: &[u8], data: &[u8]) -> [u8; CHECKSUM_LEN] {
    let mut values = hrp_expand(hrp);
    values.extend_from_slice(data);
    values.extend_from_slice(&[0; CHECKSUM_LEN]);
    let pm = polymod(&values) ^ BECH32M_CONST;
    let mut out = [0u8; CHECKSUM_LEN];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = ((pm >> (5 * (5 - i))) & 31) as u8;
    }
    out
}

fn valid_hrp(hrp: &[u8]) -> Result<(), Bech32Error> {
    if hrp.is_empty() {
        return Err(Bech32Error::EmptyHrp);
    }
    if hrp.iter().any(|&c| !(33..=126).contains(&c)) {
        return Err(Bech32Error::InvalidHrpChar);
    }
    Ok(())
}

/// Encode un préfixe et des valeurs de 5 bits.
pub fn encode(hrp: &str, data: &[u8]) -> Result<String, Bech32Error> {
    let hrp = hrp.to_ascii_lowercase();
    valid_hrp(hrp.as_bytes())?;
    if data.iter().any(|&d| d > 31) {
        return Err(Bech32Error::InvalidValue);
    }
    if hrp.len() + 1 + data.len() + CHECKSUM_LEN > MAX_LEN {
        return Err(Bech32Error::TooLong);
    }
    let check = checksum(hrp.as_bytes(), data);
    let mut s = String::with_capacity(hrp.len() + 1 + data.len() + CHECKSUM_LEN);
    s.push_str(&hrp);
    s.push('1');
    for &d in data.iter().chain(check.iter()) {
        s.push(char::from(CHARSET[usize::from(d)]));
    }
    Ok(s)
}

/// Décode une chaîne Bech32m : renvoie le préfixe (en minuscules) et les
/// valeurs de 5 bits (sans la somme de contrôle).
pub fn decode(s: &str) -> Result<(String, Vec<u8>), Bech32Error> {
    let bytes = s.as_bytes();
    if bytes.len() > MAX_LEN {
        return Err(Bech32Error::TooLong);
    }
    if bytes.iter().any(|&c| !(33..=126).contains(&c)) {
        return Err(Bech32Error::InvalidHrpChar);
    }
    let has_lower = bytes.iter().any(u8::is_ascii_lowercase);
    let has_upper = bytes.iter().any(u8::is_ascii_uppercase);
    if has_lower && has_upper {
        return Err(Bech32Error::MixedCase);
    }
    let lower = s.to_ascii_lowercase();
    let pos = lower.rfind('1').ok_or(Bech32Error::NoSeparator)?;
    if pos == 0 {
        return Err(Bech32Error::EmptyHrp);
    }
    if pos + 1 + CHECKSUM_LEN > lower.len() {
        return Err(Bech32Error::TooShortChecksum);
    }
    let (hrp, rest) = lower.split_at(pos);
    let mut data = Vec::with_capacity(rest.len() - 1);
    for c in rest.bytes().skip(1) {
        let v = CHARSET.iter().position(|&x| x == c).ok_or(Bech32Error::InvalidDataChar)?;
        data.push(v as u8);
    }
    let mut values = hrp_expand(hrp.as_bytes());
    values.extend_from_slice(&data);
    if polymod(&values) != BECH32M_CONST {
        return Err(Bech32Error::InvalidChecksum);
    }
    data.truncate(data.len() - CHECKSUM_LEN);
    Ok((hrp.to_string(), data))
}

/// Convertit entre groupes de bits (8 → 5 pour encoder, 5 → 8 pour décoder).
pub fn convert_bits(data: &[u8], from: u32, to: u32, pad: bool) -> Result<Vec<u8>, Bech32Error> {
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    let max_value: u32 = (1 << to) - 1;
    let max_acc: u32 = (1 << (from + to - 1)) - 1;
    let mut out = Vec::with_capacity(data.len() * from as usize / to as usize + 1);
    for &v in data {
        if u32::from(v) >> from != 0 {
            return Err(Bech32Error::InvalidValue);
        }
        acc = ((acc << from) | u32::from(v)) & max_acc;
        bits += from;
        while bits >= to {
            bits -= to;
            out.push(((acc >> bits) & max_value) as u8);
        }
    }
    if pad {
        if bits > 0 {
            out.push(((acc << (to - bits)) & max_value) as u8);
        }
    } else if bits >= from || ((acc << (to - bits)) & max_value) != 0 {
        return Err(Bech32Error::InvalidPadding);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bip350_valid_vectors() {
        let valid = [
            "A1LQFN3A",
            "a1lqfn3a",
            "an83characterlonghumanreadablepartthatcontainsthetheexcludedcharactersbioandnumber11sg7hg6",
            "abcdef1l7aum6echk45nj3s0wdvt2fg8x9yrzpqzd3ryx",
            "11llllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllludsr8",
            "split1checkupstagehandshakeupstreamerranterredcaperredlc445v",
            "?1v759aa",
        ];
        for s in valid {
            let (hrp, data) = decode(s).unwrap_or_else(|e| panic!("{s}: {e}"));
            // Ré-encodage identique (en minuscules).
            assert_eq!(encode(&hrp, &data).unwrap(), s.to_ascii_lowercase());
        }
    }

    #[test]
    fn bip350_invalid_vectors() {
        let invalid: [(&str, Bech32Error); 14] = [
            ("\u{20}1xj0phk", Bech32Error::InvalidHrpChar),
            ("\u{7f}1g6xzxy", Bech32Error::InvalidHrpChar),
            ("\u{80}1vctc34", Bech32Error::InvalidHrpChar),
            (
                "an84characterslonghumanreadablepartthatcontainsthetheexcludedcharactersbioandnumber11d6pts4",
                Bech32Error::TooLong,
            ),
            ("qyrz8wqd2c9m", Bech32Error::NoSeparator),
            ("1qyrz8wqd2c9m", Bech32Error::EmptyHrp),
            ("y1b0jsk6g", Bech32Error::InvalidDataChar),
            ("lt1igcx5c0", Bech32Error::InvalidDataChar),
            ("in1muywd", Bech32Error::TooShortChecksum),
            ("mm1crxm3i", Bech32Error::InvalidDataChar),
            ("au1s5cgom", Bech32Error::InvalidDataChar),
            ("M1VUXWEZ", Bech32Error::InvalidChecksum),
            ("16plkw9", Bech32Error::EmptyHrp),
            ("1p2gdwpf", Bech32Error::EmptyHrp),
        ];
        for (s, expected) in invalid {
            assert_eq!(decode(s), Err(expected), "{s:?}");
        }
    }

    #[test]
    fn mixed_case_rejected() {
        assert_eq!(decode("A1lqfn3a"), Err(Bech32Error::MixedCase));
    }

    #[test]
    fn convert_bits_roundtrip() {
        let data: Vec<u8> = (0..=255).collect();
        let five = convert_bits(&data, 8, 5, true).unwrap();
        assert!(five.iter().all(|&v| v < 32));
        assert_eq!(convert_bits(&five, 5, 8, false).unwrap(), data);
        // Remplissage non nul refusé.
        assert_eq!(convert_bits(&[31], 5, 8, false), Err(Bech32Error::InvalidPadding));
    }
}
