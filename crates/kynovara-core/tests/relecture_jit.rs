//! Relecture indépendante de la 0.4.0 : RandomX quand le système refuse de
//! rendre de la mémoire exécutable (compilateur à la volée, « JIT »).
//!
//! Ces trois tests passent dans un environnement ordinaire. Lancés un par un
//! sous un profil seccomp qui refuse `mprotect(…, PROT_EXEC)` (ce que font
//! `MemoryDenyWriteExecute=yes` de systemd, SELinux `deny_execmem` ou PaX),
//! ils montrent que :
//! - la création d'un cache avec JIT ne signale PAS ce refus : la version de
//!   RandomX livrée avec `randomx-rs` 1.6.0 ignore l'erreur de `mprotect`
//!   (`setPagesRX`/`setPagesRWX` dans `virtual_memory.c`) ;
//! - le repli sur l'interpréteur de `pow.rs` (`build_vm`), qui n'est tenté
//!   que si la création du cache échoue, ne se déclenche donc jamais ;
//! - le calculateur du nœud (`RandomXHasher`) fait alors mourir le processus
//!   (SIGSEGV) dès la première empreinte, alors que l'interpréteur aurait
//!   donné la bonne.
//!
//! Correction (constat V-2 d'AUDIT.md) : la bibliothèque n'étant pas
//! modifiable, `kynovarad` essaie le JIT dans un processus fils au démarrage
//! (`pow::self_test`) et passe à l'interpréteur s'il échoue ;
//! `KYNOVARA_RANDOMX_JIT=0` le désactive d'office. Sous le profil seccomp, ces
//! tests passent tous avec `KYNOVARA_RANDOMX_JIT=0`. Ils restent ici pour
//! documenter le comportement de la bibliothèque.
#![allow(clippy::unwrap_used)]

use kynovara_core::hash::Hash256;
use kynovara_core::hex;
use kynovara_core::pow::RandomXHasher;
use randomx_rs::{RandomXCache, RandomXFlag, RandomXVM};

const KEY: Hash256 = Hash256([0x42; 32]);
const INPUT: &[u8] = b"Kynovara : relecture du repli sans JIT";

/// Empreinte RandomX calculée par l'interpréteur seul (sans JIT).
fn interpreter_hash(key: &[u8], input: &[u8]) -> Vec<u8> {
    let mut flags = RandomXFlag::get_recommended_flags();
    flags.remove(RandomXFlag::FLAG_JIT);
    let cache = RandomXCache::new(flags, key).unwrap();
    let vm = RandomXVM::new(flags, Some(cache), None).unwrap();
    vm.calculate_hash(input).unwrap()
}

#[test]
fn relecture_jit_le_cache_ne_signale_pas_un_refus_de_memoire_executable() {
    // Avec JIT (drapeaux recommandés, comme `pow.rs`), la création du cache
    // réussit même quand `mprotect(PROT_EXEC)` est refusé : rien ne permet à
    // `build_vm` de passer à l'interpréteur.
    let flags = RandomXFlag::get_recommended_flags();
    assert!(flags.contains(RandomXFlag::FLAG_JIT), "machine sans JIT : test sans objet");
    assert!(RandomXCache::new(flags, KEY.as_bytes()).is_ok());
}

#[test]
fn relecture_jit_le_calculateur_du_noeud_se_cree_sans_erreur() {
    // `RandomXHasher::new` (cache + machine virtuelle avec JIT) réussit aussi
    // sous le profil seccomp : le nœud n'a aucun moyen de voir le problème
    // avant le premier calcul.
    assert_eq!(RandomXHasher::new(&KEY).unwrap().key(), &KEY);
}

#[test]
fn relecture_jit_l_interpreteur_donne_le_vecteur_officiel() {
    // Vecteur du dépôt tevador/RandomX : l'interpréteur fonctionne partout, y
    // compris sous le profil seccomp.
    assert_eq!(
        hex::encode(&interpreter_hash(b"test key 000", b"This is a test")),
        "639183aae1bf4c9a35884cb46b09cad9175f04efd7684e7262a0ac1c2f0b4e3f"
    );
}

#[test]
fn relecture_jit_le_calculateur_du_noeud_egale_l_interpreteur() {
    // Ce que fait le nœud (cache puis machine virtuelle avec JIT). Sous le
    // profil seccomp, le processus meurt ici (SIGSEGV) au lieu de se replier.
    let hasher = RandomXHasher::new(&KEY).unwrap();
    let got = hasher.hash(INPUT).unwrap();
    assert_eq!(got.0.to_vec(), interpreter_hash(KEY.as_bytes(), INPUT));
}

#[test]
fn relecture_jit_la_verification_d_un_bloc_passe_par_le_meme_chemin() {
    // Fil de vérification partagé (`randomx_meets_target` dans chain.rs) :
    // même construction, donc même plantage sous le profil seccomp, au premier
    // bloc RandomX reçu.
    let got = kynovara_core::pow::randomx_hash(&KEY, INPUT).unwrap();
    assert_eq!(got.0.to_vec(), interpreter_hash(KEY.as_bytes(), INPUT));
}
