//! Entier non signé de 256 bits.
//!
//! Sert aux cibles de difficulté (un hachage de bloc, lu comme un nombre de
//! 256 bits en gros-boutiste, doit être ≤ à la cible) et au travail cumulé de
//! la chaîne. Toutes les opérations sont déterministes et vérifiées : aucun
//! débordement silencieux.

use crate::hex::{self, HexError};
use std::cmp::Ordering;
use std::fmt;

/// Entier de 256 bits (quatre mots de 64 bits, le mot 0 est le poids faible).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct U256([u64; 4]);

impl U256 {
    pub const ZERO: U256 = U256([0; 4]);
    pub const ONE: U256 = U256([1, 0, 0, 0]);
    pub const MAX: U256 = U256([u64::MAX; 4]);

    /// Construit à partir d'un `u64`.
    pub const fn from_u64(v: u64) -> Self {
        U256([v, 0, 0, 0])
    }

    /// 2 à la puissance `n` (`n` < 256).
    pub const fn pow2(n: u32) -> Self {
        assert!(n < 256, "exposant hors limites");
        let mut limbs = [0u64; 4];
        limbs[(n / 64) as usize] = 1u64 << (n % 64);
        U256(limbs)
    }

    /// 2 à la puissance `n`, moins 1 : les `n` bits de poids faible à 1
    /// (`n` ≤ 256).
    pub const fn low_bits(n: u32) -> Self {
        assert!(n <= 256, "nombre de bits hors limites");
        let mut limbs = [0u64; 4];
        let mut i = 0;
        while i < 4 {
            let start = i as u32 * 64;
            if n >= start + 64 {
                limbs[i] = u64::MAX;
            } else if n > start {
                limbs[i] = (1u64 << (n - start)) - 1;
            }
            i += 1;
        }
        U256(limbs)
    }

    /// Lecture de 32 octets en gros-boutiste.
    pub fn from_be_bytes(b: [u8; 32]) -> Self {
        let mut limbs = [0u64; 4];
        for (i, chunk) in b.chunks_exact(8).enumerate() {
            let mut word = [0u8; 8];
            word.copy_from_slice(chunk);
            limbs[3 - i] = u64::from_be_bytes(word);
        }
        U256(limbs)
    }

    /// Écriture sur 32 octets en gros-boutiste.
    pub fn to_be_bytes(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        for i in 0..4 {
            out[i * 8..i * 8 + 8].copy_from_slice(&self.0[3 - i].to_be_bytes());
        }
        out
    }

    /// Représentation hexadécimale sur 64 caractères.
    pub fn to_hex(&self) -> String {
        hex::encode(&self.to_be_bytes())
    }

    /// Lecture hexadécimale (64 caractères).
    pub fn from_hex(s: &str) -> Result<Self, HexError> {
        Ok(U256::from_be_bytes(hex::decode_array(s)?))
    }

    pub fn is_zero(&self) -> bool {
        self.0 == [0; 4]
    }

    /// Les 64 bits de poids faible.
    pub fn low_u64(&self) -> u64 {
        self.0[0]
    }

    /// Nombre de bits significatifs (0 pour zéro).
    pub fn bits(&self) -> u32 {
        for i in (0..4).rev() {
            if self.0[i] != 0 {
                return i as u32 * 64 + (64 - self.0[i].leading_zeros());
            }
        }
        0
    }

    /// Valeur du bit `i` (0 = poids faible).
    pub fn bit(&self, i: u32) -> bool {
        i < 256 && (self.0[(i / 64) as usize] >> (i % 64)) & 1 == 1
    }

    fn set_bit(&mut self, i: u32) {
        self.0[(i / 64) as usize] |= 1u64 << (i % 64);
    }

    /// Addition vérifiée.
    pub fn checked_add(self, other: U256) -> Option<U256> {
        let mut out = [0u64; 4];
        let mut carry = false;
        for (i, slot) in out.iter_mut().enumerate() {
            let (s1, c1) = self.0[i].overflowing_add(other.0[i]);
            let (s2, c2) = s1.overflowing_add(u64::from(carry));
            *slot = s2;
            carry = c1 || c2;
        }
        if carry { None } else { Some(U256(out)) }
    }

    /// Addition saturée à `U256::MAX`.
    pub fn saturating_add(self, other: U256) -> U256 {
        self.checked_add(other).unwrap_or(U256::MAX)
    }

    /// Soustraction vérifiée.
    pub fn checked_sub(self, other: U256) -> Option<U256> {
        let mut out = [0u64; 4];
        let mut borrow = false;
        for (i, slot) in out.iter_mut().enumerate() {
            let (d1, b1) = self.0[i].overflowing_sub(other.0[i]);
            let (d2, b2) = d1.overflowing_sub(u64::from(borrow));
            *slot = d2;
            borrow = b1 || b2;
        }
        if borrow { None } else { Some(U256(out)) }
    }

    /// Multiplication vérifiée par un `u64`.
    pub fn checked_mul_u64(self, m: u64) -> Option<U256> {
        let mut out = [0u64; 4];
        let mut carry: u128 = 0;
        for (i, slot) in out.iter_mut().enumerate() {
            let p = u128::from(self.0[i]) * u128::from(m) + carry;
            *slot = p as u64;
            carry = p >> 64;
        }
        if carry != 0 { None } else { Some(U256(out)) }
    }

    /// Division entière par un `u64` non nul (le reste est ignoré).
    ///
    /// # Panique
    /// Si `d == 0` (erreur de programmation : jamais appelé ainsi).
    pub fn div_u64(self, d: u64) -> U256 {
        assert!(d != 0, "division par zéro");
        let mut out = [0u64; 4];
        let mut rem: u128 = 0;
        for i in (0..4).rev() {
            let cur = (rem << 64) | u128::from(self.0[i]);
            out[i] = (cur / u128::from(d)) as u64;
            rem = cur % u128::from(d);
        }
        U256(out)
    }

    /// Division euclidienne vérifiée : `None` si le diviseur est nul.
    pub fn checked_div_rem(self, divisor: U256) -> Option<(U256, U256)> {
        if divisor.is_zero() {
            return None;
        }
        let mut quotient = U256::ZERO;
        let mut rem = U256::ZERO;
        for i in (0..self.bits()).rev() {
            // Si le bit de poids fort est à 1, le décalage produit (en vrai)
            // un nombre de 257 bits, forcément ≥ au diviseur.
            let overflow = rem.bit(255);
            rem = rem << 1;
            if self.bit(i) {
                rem.0[0] |= 1;
            }
            if overflow || rem >= divisor {
                rem = rem.wrapping_sub(divisor);
                quotient.set_bit(i);
            }
        }
        Some((quotient, rem))
    }

    /// Soustraction modulo 2^256.
    pub fn wrapping_sub(self, other: U256) -> U256 {
        let mut out = [0u64; 4];
        let mut borrow = false;
        for (i, slot) in out.iter_mut().enumerate() {
            let (d1, b1) = self.0[i].overflowing_sub(other.0[i]);
            let (d2, b2) = d1.overflowing_sub(u64::from(borrow));
            *slot = d2;
            borrow = b1 || b2;
        }
        U256(out)
    }

    /// Décalage à gauche (les bits sortants sont perdus).
    fn shift_left(self, n: u32) -> U256 {
        if n >= 256 {
            return U256::ZERO;
        }
        let words = (n / 64) as usize;
        let bits = n % 64;
        let mut out = [0u64; 4];
        for i in (words..4).rev() {
            let src = i - words;
            out[i] = self.0[src] << bits;
            if bits > 0 && src > 0 {
                out[i] |= self.0[src - 1] >> (64 - bits);
            }
        }
        U256(out)
    }

    /// Décalage à droite.
    fn shift_right(self, n: u32) -> U256 {
        if n >= 256 {
            return U256::ZERO;
        }
        let words = (n / 64) as usize;
        let bits = n % 64;
        let mut out = [0u64; 4];
        for (i, slot) in out.iter_mut().enumerate().take(4 - words) {
            let src = i + words;
            *slot = self.0[src] >> bits;
            if bits > 0 && src + 1 < 4 {
                *slot |= self.0[src + 1] << (64 - bits);
            }
        }
        U256(out)
    }

    /// Approximation en virgule flottante (affichage seulement, jamais pour
    /// le consensus).
    pub fn to_f64(&self) -> f64 {
        let mut v = 0.0f64;
        for i in (0..4).rev() {
            v = v * 18_446_744_073_709_551_616.0 + self.0[i] as f64;
        }
        v
    }
}

impl std::ops::Shl<u32> for U256 {
    type Output = U256;
    fn shl(self, n: u32) -> U256 {
        self.shift_left(n)
    }
}

impl std::ops::Shr<u32> for U256 {
    type Output = U256;
    fn shr(self, n: u32) -> U256 {
        self.shift_right(n)
    }
}

impl std::ops::Not for U256 {
    type Output = U256;
    fn not(self) -> U256 {
        U256([!self.0[0], !self.0[1], !self.0[2], !self.0[3]])
    }
}

impl Ord for U256 {
    fn cmp(&self, other: &Self) -> Ordering {
        for i in (0..4).rev() {
            match self.0[i].cmp(&other.0[i]) {
                Ordering::Equal => continue,
                ord => return ord,
            }
        }
        Ordering::Equal
    }
}

impl PartialOrd for U256 {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Debug for U256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "U256(0x{})", self.to_hex())
    }
}

impl fmt::Display for U256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// Travail représenté par une cible : nombre moyen de hachages nécessaires
/// pour trouver un bloc, soit `2^256 / (cible + 1)`.
///
/// Règle de consensus : la chaîne active est celle qui a le plus grand travail
/// cumulé (et non la plus longue).
pub fn work_from_target(target: U256) -> U256 {
    match target.checked_add(U256::ONE) {
        // cible = 2^256 - 1 : travail minimal de 1.
        None => U256::ONE,
        // (2^256 - 1 - cible) / (cible + 1) + 1 == 2^256 / (cible + 1)
        Some(denominator) => {
            let (q, _) = (!target).checked_div_rem(denominator).unwrap_or((U256::ZERO, U256::ZERO));
            q.saturating_add(U256::ONE)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(v: u128) -> U256 {
        U256([v as u64, (v >> 64) as u64, 0, 0])
    }

    /// Générateur pseudo-aléatoire déterministe (tests uniquement).
    fn splitmix(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    #[test]
    fn bytes_roundtrip_and_order() {
        let mut b = [0u8; 32];
        b[0] = 0x80;
        b[31] = 1;
        let x = U256::from_be_bytes(b);
        assert_eq!(x.to_be_bytes(), b);
        assert_eq!(x.bits(), 256);
        assert!(x > U256::pow2(254));
        assert!(U256::ONE < U256::pow2(1));
        assert_eq!(U256::from_hex(&x.to_hex()).unwrap(), x);
    }

    #[test]
    fn arithmetic_matches_u128() {
        let mut s = 42u64;
        for _ in 0..10_000 {
            let a = (u128::from(splitmix(&mut s)) << 32) ^ u128::from(splitmix(&mut s));
            let b = u128::from(splitmix(&mut s)) | 1;
            let m = splitmix(&mut s) >> 40;
            assert_eq!(u(a).checked_add(u(b)), a.checked_add(b).map(u));
            assert_eq!(u(a).checked_sub(u(b)), a.checked_sub(b).map(u));
            assert_eq!(u(a).checked_mul_u64(m), Some(u(a * u128::from(m))));
            assert_eq!(u(a).div_u64(b as u64 | 1), u(a / u128::from(b as u64 | 1)));
            let (q, r) = u(a).checked_div_rem(u(b)).unwrap();
            assert_eq!((q, r), (u(a / b), u(a % b)));
            let n = (splitmix(&mut s) % 60) as u32;
            assert_eq!((u(a >> 4) << n) >> n, u(a >> 4));
            assert_eq!(u(a) >> n, u(a >> n));
            assert_eq!(u(a).cmp(&u(b)), a.cmp(&b));
        }
    }

    #[test]
    fn shifts_across_words() {
        let x = U256::pow2(63);
        assert_eq!(x << 1, U256::pow2(64));
        assert_eq!(x << 130, U256::pow2(193));
        assert_eq!(U256::pow2(200) >> 137, U256::pow2(63));
        assert_eq!(U256::MAX << 256, U256::ZERO);
        assert_eq!(U256::MAX >> 255, U256::ONE);
        assert_eq!(U256::MAX << 255, U256::pow2(255));
        assert_eq!(!U256::ZERO, U256::MAX);
    }

    #[test]
    fn overflow_detection() {
        assert_eq!(U256::MAX.checked_add(U256::ONE), None);
        assert_eq!(U256::ZERO.checked_sub(U256::ONE), None);
        assert_eq!(U256::pow2(255).checked_mul_u64(2), None);
        assert_eq!(U256::pow2(254).checked_mul_u64(2), Some(U256::pow2(255)));
        assert_eq!(U256::ONE.checked_div_rem(U256::ZERO), None);
        assert_eq!(U256::MAX.saturating_add(U256::ONE), U256::MAX);
        // Diviseur > 2^255 : le reste intermédiaire dépasse 256 bits.
        let d = U256::pow2(255).checked_add(U256::ONE).unwrap();
        let (q, r) = U256::MAX.checked_div_rem(d).unwrap();
        assert_eq!(q, U256::ONE);
        assert_eq!(r, U256::pow2(255).checked_sub(U256::from_u64(2)).unwrap());
        let (q, r) = U256::MAX.checked_div_rem(U256::MAX).unwrap();
        assert_eq!((q, r), (U256::ONE, U256::ZERO));
        assert_eq!(U256::ZERO.wrapping_sub(U256::ONE), U256::MAX);
    }

    #[test]
    fn work_values() {
        // Cible 2^255 - 1 : un hachage sur deux réussit, travail = 2.
        let t = U256::pow2(255).checked_sub(U256::ONE).unwrap();
        assert_eq!(work_from_target(t), U256::from_u64(2));
        // Cible 2^224 - 1 : travail = 2^32.
        let t = U256::pow2(224).checked_sub(U256::ONE).unwrap();
        assert_eq!(work_from_target(t), U256::pow2(32));
        assert_eq!(work_from_target(U256::MAX), U256::ONE);
        // Plus la cible est basse, plus le travail est grand.
        assert!(work_from_target(U256::pow2(200)) > work_from_target(U256::pow2(201)));
    }
}
