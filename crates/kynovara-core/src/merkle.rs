//! Arbre de Merkle des transactions d'un bloc.
//!
//! Différences voulues avec Bitcoin (règles de consensus) :
//! - feuilles et nœuds internes sont hachés avec des étiquettes différentes :
//!   un nœud interne ne peut pas se faire passer pour une transaction ;
//! - un nœud sans partenaire est **remonté tel quel** au lieu d'être dupliqué.
//!   La duplication de Bitcoin permet à deux listes de transactions
//!   différentes d'avoir la même racine (CVE-2012-2459) ; ce n'est pas le cas
//!   ici.

use crate::hash::{Hash256, tagged_hash, tags};

/// Racine de Merkle d'une liste d'identifiants (empreinte nulle si vide).
pub fn merkle_root(leaves: &[Hash256]) -> Hash256 {
    if leaves.is_empty() {
        return Hash256::ZERO;
    }
    let mut level: Vec<Hash256> = leaves.iter().map(|leaf| tagged_hash(tags::MERKLE_LEAF, &[&leaf.0])).collect();
    while level.len() > 1 {
        level = level
            .chunks(2)
            .map(|pair| match pair {
                [a, b] => tagged_hash(tags::MERKLE_NODE, &[&a.0, &b.0]),
                [a] => *a,
                _ => Hash256::ZERO,
            })
            .collect();
    }
    level[0]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(i: u8) -> Hash256 {
        Hash256([i; 32])
    }

    #[test]
    fn structure() {
        let leaf = |x: Hash256| tagged_hash(tags::MERKLE_LEAF, &[&x.0]);
        let node = |a: Hash256, b: Hash256| tagged_hash(tags::MERKLE_NODE, &[&a.0, &b.0]);
        assert_eq!(merkle_root(&[]), Hash256::ZERO);
        assert_eq!(merkle_root(&[h(1)]), leaf(h(1)));
        assert_eq!(merkle_root(&[h(1), h(2)]), node(leaf(h(1)), leaf(h(2))));
        assert_eq!(merkle_root(&[h(1), h(2), h(3)]), node(node(leaf(h(1)), leaf(h(2))), leaf(h(3))));
    }

    #[test]
    fn duplication_does_not_collide() {
        // La faille CVE-2012-2459 : [a, b, c] et [a, b, c, c] ont la même
        // racine dans Bitcoin. Ici, non.
        assert_ne!(merkle_root(&[h(1), h(2), h(3)]), merkle_root(&[h(1), h(2), h(3), h(3)]));
        assert_ne!(merkle_root(&[h(1)]), merkle_root(&[h(1), h(1)]));
    }

    #[test]
    fn order_matters() {
        assert_ne!(merkle_root(&[h(1), h(2)]), merkle_root(&[h(2), h(1)]));
    }

    #[test]
    fn internal_node_is_not_a_leaf() {
        // Une feuille égale à un nœud interne ne reproduit pas la racine.
        let a = merkle_root(&[h(1), h(2)]);
        assert_ne!(merkle_root(&[a]), a);
    }
}
