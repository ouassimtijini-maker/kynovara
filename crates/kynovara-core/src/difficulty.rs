//! Ajustement de la difficulté : algorithme LWMA (moyenne mobile pondérée
//! linéairement, de zawy12), utilisé par de nombreuses petites chaînes à
//! preuve de travail.
//!
//! Principe : la cible du prochain bloc est la moyenne des cibles des `N`
//! derniers blocs, multipliée par le rapport « temps réellement écoulé /
//! temps visé », où les blocs récents comptent davantage. Si les blocs
//! arrivent trop vite, la cible baisse (c'est plus difficile) ; trop
//! lentement, elle monte.
//!
//! Protections intégrées (règles de consensus) :
//! - un temps de bloc est plafonné à `6 × T` : une longue pause ne fait pas
//!   s'effondrer la difficulté ;
//! - un horodatage qui recule est traité comme « +1 seconde » ;
//! - la cible ne dépasse jamais `pow_limit` et ne vaut jamais 0.
//!
//! Avant que `N` blocs existent, la cible du bloc de genèse est utilisée.

use crate::params::ConsensusParams;
use crate::u256::U256;

/// Cible du bloc suivant.
///
/// `parent_height` est la hauteur du dernier bloc. `window` contient
/// `(horodatage, cible)` des `N + 1` derniers blocs (le plus ancien d'abord,
/// le parent en dernier) ; il n'est lu que si `parent_height >= N`.
pub fn next_target(params: &ConsensusParams, parent_height: u64, window: &[(u64, U256)]) -> U256 {
    if params.no_retarget {
        return params.pow_limit;
    }
    let n = params.lwma_window;
    if parent_height < n || window.len() as u64 != n + 1 {
        return params.genesis.target;
    }
    let t = params.target_spacing;
    // k = N(N+1)T/2 ramène la somme pondérée des temps à une moyenne.
    let k = n * (n + 1) * t / 2;
    let mut previous_timestamp = window[0].0;
    let mut weighted_solvetimes: u64 = 0;
    let mut average_target = U256::ZERO;
    for (j, &(timestamp, target)) in window[1..].iter().enumerate() {
        let this_timestamp =
            if timestamp > previous_timestamp { timestamp } else { previous_timestamp.saturating_add(1) };
        let solvetime = (this_timestamp - previous_timestamp).min(6 * t);
        previous_timestamp = this_timestamp;
        weighted_solvetimes += solvetime * (j as u64 + 1);
        // Diviser avant d'additionner évite tout débordement.
        average_target = average_target.saturating_add(target.div_u64(n).div_u64(k));
    }
    let next = average_target.checked_mul_u64(weighted_solvetimes).unwrap_or(params.pow_limit);
    next.min(params.pow_limit).max(U256::ONE)
}

/// Difficulté lisible : rapport entre la cible minimale et la cible actuelle
/// (1.0 = difficulté minimale). Pour l'affichage uniquement.
pub fn difficulty(params: &ConsensusParams, target: &U256) -> f64 {
    let t = target.to_f64();
    if t <= 0.0 { f64::INFINITY } else { params.pow_limit.to_f64() / t }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{REGTEST, TESTNET};

    fn window(times: &[u64], target: U256) -> Vec<(u64, U256)> {
        times.iter().map(|&t| (t, target)).collect()
    }

    fn regular(n: u64, spacing: u64) -> Vec<u64> {
        (0..=n).map(|i| 1_000_000 + i * spacing).collect()
    }

    fn ratio(a: U256, b: U256) -> f64 {
        a.to_f64() / b.to_f64()
    }

    #[test]
    fn regtest_never_retargets() {
        assert_eq!(next_target(&REGTEST, 5000, &[]), REGTEST.pow_limit);
    }

    #[test]
    fn early_blocks_use_genesis_target() {
        let p = &TESTNET;
        assert_eq!(next_target(p, 0, &[]), p.genesis.target);
        assert_eq!(next_target(p, p.lwma_window - 1, &[]), p.genesis.target);
    }

    #[test]
    fn on_schedule_keeps_target() {
        let p = &TESTNET;
        let target = U256::low_bits(220);
        let w = window(&regular(p.lwma_window, p.target_spacing), target);
        let r = ratio(next_target(p, 1000, &w), target);
        assert!((r - 1.0).abs() < 1e-6, "{r}");
    }

    #[test]
    fn fast_blocks_raise_difficulty_slow_blocks_lower_it() {
        let p = &TESTNET;
        let target = U256::low_bits(220);
        let fast = window(&regular(p.lwma_window, p.target_spacing / 2), target);
        let r = ratio(next_target(p, 1000, &fast), target);
        assert!((r - 0.5).abs() < 1e-6, "{r}");
        let slow = window(&regular(p.lwma_window, p.target_spacing * 2), target);
        let r = ratio(next_target(p, 1000, &slow), target);
        assert!((r - 2.0).abs() < 1e-6, "{r}");
    }

    #[test]
    fn long_pause_is_capped() {
        let p = &TESTNET;
        let target = U256::low_bits(220);
        // Tous les blocs à l'heure sauf le dernier, arrivé après 10 jours.
        let mut times = regular(p.lwma_window, p.target_spacing);
        let last = times.len() - 1;
        times[last] = times[last - 1] + 864_000;
        let r = ratio(next_target(p, 1000, &window(&times, target)), target);
        // Le temps est plafonné à 6T : hausse limitée (bien moins que ×2).
        assert!(r > 1.0 && r < 1.2, "{r}");
    }

    #[test]
    fn backwards_timestamps_are_safe() {
        let p = &TESTNET;
        let target = U256::low_bits(220);
        let times: Vec<u64> = (0..=p.lwma_window).map(|i| 1_000_000 - i).collect();
        let next = next_target(p, 1000, &window(&times, target));
        assert!(next >= U256::ONE && next < target);
    }

    #[test]
    fn result_is_clamped_to_pow_limit() {
        let p = &TESTNET;
        let w = window(&regular(p.lwma_window, p.target_spacing * 6), p.pow_limit);
        assert_eq!(next_target(p, 1000, &w), p.pow_limit);
    }

    #[test]
    fn wrong_window_size_falls_back() {
        let p = &TESTNET;
        assert_eq!(next_target(p, 1000, &[(1, U256::ONE)]), p.genesis.target);
    }
}
