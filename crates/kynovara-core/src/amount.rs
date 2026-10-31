//! Montants.
//!
//! En interne, tout montant est un entier de « unités » (comme les satoshis
//! de Bitcoin) : 1 KYNO = 100 000 000 unités. Aucun nombre à virgule flottante
//! n'est jamais utilisé pour de l'argent.

use std::fmt;

/// Symbole boursier (provisoire, voir docs/DECISIONS.md).
pub const TICKER: &str = "KYNO";
/// Nombre de décimales.
pub const DECIMALS: usize = 8;
/// Nombre d'unités dans 1 KYNO.
pub const COIN: u64 = 100_000_000;
/// Borne de sécurité sur tout montant (règle de consensus). Elle est
/// supérieure à l'émission totale prévue (environ 105 millions de KYNO).
pub const MAX_MONEY: u64 = 110_000_000 * COIN;

/// Erreur de lecture de montant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AmountError {
    Empty,
    InvalidFormat,
    TooManyDecimals,
    TooLarge,
    Zero,
}

impl fmt::Display for AmountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            AmountError::Empty => "montant vide",
            AmountError::InvalidFormat => "format de montant invalide (exemple : 12.5)",
            AmountError::TooManyDecimals => "trop de décimales (8 au maximum)",
            AmountError::TooLarge => "montant trop grand",
            AmountError::Zero => "le montant doit être supérieur à zéro",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for AmountError {}

/// Lit un montant en KYNO (« 12 », « 12.5 » ou « 12,5 ») et le convertit en
/// unités. Refuse les signes, les exposants, les espaces et plus de 8
/// décimales.
pub fn parse_amount(s: &str) -> Result<u64, AmountError> {
    if s.is_empty() {
        return Err(AmountError::Empty);
    }
    if s.len() > 40 {
        return Err(AmountError::TooLarge);
    }
    let normalized = s.replace(',', ".");
    let (int_part, frac_part) = match normalized.split_once('.') {
        Some((i, f)) => {
            if f.is_empty() || f.contains('.') {
                return Err(AmountError::InvalidFormat);
            }
            (i, f)
        }
        None => (normalized.as_str(), ""),
    };
    if int_part.is_empty()
        || !int_part.bytes().all(|c| c.is_ascii_digit())
        || !frac_part.bytes().all(|c| c.is_ascii_digit())
    {
        return Err(AmountError::InvalidFormat);
    }
    if frac_part.len() > DECIMALS {
        return Err(AmountError::TooManyDecimals);
    }
    let int_value: u64 = int_part.parse().map_err(|_| AmountError::TooLarge)?;
    let mut frac_value: u64 = 0;
    for i in 0..DECIMALS {
        let digit = frac_part.as_bytes().get(i).map_or(0, |c| u64::from(c - b'0'));
        frac_value = frac_value * 10 + digit;
    }
    let total = int_value.checked_mul(COIN).and_then(|v| v.checked_add(frac_value)).ok_or(AmountError::TooLarge)?;
    if total > MAX_MONEY {
        return Err(AmountError::TooLarge);
    }
    Ok(total)
}

/// Lit un montant strictement positif.
pub fn parse_positive_amount(s: &str) -> Result<u64, AmountError> {
    match parse_amount(s)? {
        0 => Err(AmountError::Zero),
        v => Ok(v),
    }
}

/// Affiche un montant en KYNO sans zéros inutiles (« 12.5 », « 0.00000001 »).
pub fn format_amount(units: u64) -> String {
    let int = units / COIN;
    let frac = units % COIN;
    if frac == 0 {
        format!("{int}")
    } else {
        let f = format!("{frac:08}");
        format!("{int}.{}", f.trim_end_matches('0'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid() {
        assert_eq!(parse_amount("0").unwrap(), 0);
        assert_eq!(parse_amount("1").unwrap(), COIN);
        assert_eq!(parse_amount("12.5").unwrap(), 1_250_000_000);
        assert_eq!(parse_amount("12,5").unwrap(), 1_250_000_000);
        assert_eq!(parse_amount("0.00000001").unwrap(), 1);
        assert_eq!(parse_amount("007.10").unwrap(), 710_000_000);
        assert_eq!(parse_amount("110000000").unwrap(), MAX_MONEY);
    }

    #[test]
    fn parse_invalid() {
        for bad in ["", ".", "1.", ".5", "-1", "+1", "1e3", " 1", "1 ", "1.2.3", "1,2.3", "abc", "0x10"] {
            assert!(parse_amount(bad).is_err(), "{bad:?} aurait dû être refusé");
        }
        assert_eq!(parse_amount("0.000000001"), Err(AmountError::TooManyDecimals));
        assert_eq!(parse_amount("110000000.00000001"), Err(AmountError::TooLarge));
        assert_eq!(parse_amount("99999999999999999999"), Err(AmountError::TooLarge));
        assert_eq!(parse_positive_amount("0.0"), Err(AmountError::Zero));
    }

    #[test]
    fn format_roundtrip() {
        for v in [0, 1, 10, COIN, COIN + 1, 1_250_000_000, MAX_MONEY, 123_456_789] {
            assert_eq!(parse_amount(&format_amount(v)).unwrap(), v);
        }
        assert_eq!(format_amount(1_250_000_000), "12.5");
        assert_eq!(format_amount(1), "0.00000001");
        assert_eq!(format_amount(5 * COIN), "5");
    }
}
