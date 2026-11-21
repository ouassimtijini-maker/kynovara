//! Preuve de travail RandomX.
//!
//! Deux algorithmes coexistent, selon la hauteur du bloc (voir
//! [`ConsensusParams::pow_algorithm`](crate::params::ConsensusParams::pow_algorithm)) :
//!
//! - **SHA-256 double** de l'en-tête, comme Bitcoin : regtest, et testnet
//!   avant l'activation de RandomX. Vérifié sans contexte
//!   ([`BlockHeader::meets_target`]).
//! - **RandomX**, l'algorithme de Monero, conçu pour les processeurs
//!   ordinaires : chaque essai exécute des programmes aléatoires dans une
//!   machine virtuelle qui exige beaucoup de mémoire, ce qui ôte presque tout
//!   avantage aux circuits spécialisés (ASIC). La preuve est valide si
//!   l'empreinte RandomX des 116 octets de l'en-tête, lue comme un nombre de
//!   256 bits en gros-boutiste, est inférieure ou égale à la cible.
//!
//! **Clé RandomX** (règle de consensus) : l'identifiant d'un bloc plus ancien
//! **de la même branche**, qui change tous les `key_interval` blocs
//! ([`ConsensusParams::randomx_key_height`](crate::params::ConsensusParams::randomx_key_height)) ;
//! la genèse elle-même utilise [`GENESIS_RANDOMX_KEY`]. La vérification
//! dépend donc du contexte : elle est faite dans `chain.rs`, une fois le
//! parent connu.
//!
//! L'identifiant d'un bloc reste le double SHA-256 de son en-tête, quel que
//! soit l'algorithme : il est bon marché et sert partout (parents,
//! inventaires, index).
//!
//! **Mise en œuvre** : l'implémentation C++ de référence (tevador/RandomX),
//! via `randomx-rs`, en **mode léger** : un cache de 256 Mio par clé (préparé
//! en un tiers de seconde environ) et environ 15 ms par empreinte. Le mode
//! rapide (2 Gio partagés entre les fils) donne les mêmes empreintes environ
//! dix fois plus vite, mais `randomx-rs` ne permet pas de partager ces objets
//! entre fils d'exécution sans code `unsafe`, interdit dans Kynovara. Chaque
//! fil qui calcule possède donc les siens : les vérifications passent par un
//! fil dédié ([`randomx_hash`]) qui garde les caches des deux dernières clés,
//! et chaque fil de minage crée son propre [`RandomXHasher`].
//!
//! **Compilateur à la volée (JIT)** : RandomX compile ses programmes en code
//! machine, ce qui exige de la mémoire exécutable. Certains systèmes durcis
//! la refusent (systemd `MemoryDenyWriteExecute=yes`, SELinux
//! `deny_execmem`, PaX), et la bibliothèque ne détecte pas ce refus : le
//! processus plante au premier calcul. `kynovarad` essaie donc le JIT dans un
//! processus fils ([`self_test`]) et passe à l'interpréteur (mêmes
//! empreintes, plus lent) s'il échoue ; la variable d'environnement
//! [`JIT_ENV`] le désactive d'office.

use crate::block::{BlockHeader, hash_meets_target};
use crate::hash::Hash256;
use randomx_rs::{RandomXCache, RandomXFlag, RandomXVM};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Once, OnceLock};

/// Variable d'environnement qui désactive le compilateur à la volée de
/// RandomX : `0`, `off`, `non`, `no` ou `false`.
pub const JIT_ENV: &str = "KYNOVARA_RANDOMX_JIT";

static JIT_DISABLED: AtomicBool = AtomicBool::new(false);
static JIT_ENV_READ: Once = Once::new();

/// Le compilateur à la volée est-il permis (pour les calculateurs créés à
/// partir de maintenant) ?
pub fn jit_allowed() -> bool {
    JIT_ENV_READ.call_once(|| {
        let off = std::env::var(JIT_ENV)
            .is_ok_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "off" | "non" | "no" | "false"));
        if off {
            JIT_DISABLED.store(true, Ordering::SeqCst);
        }
    });
    !JIT_DISABLED.load(Ordering::SeqCst)
}

/// Permet ou interdit le compilateur à la volée pour les calculateurs créés
/// ensuite (à appeler au démarrage, avant tout calcul).
pub fn set_jit_allowed(allowed: bool) {
    JIT_ENV_READ.call_once(|| {});
    JIT_DISABLED.store(!allowed, Ordering::SeqCst);
}

/// Calcule le premier vecteur officiel de RandomX avec les réglages actuels :
/// vrai si l'empreinte est la bonne. **Peut faire planter le processus** si
/// le système refuse la mémoire exécutable au JIT : à lancer dans un
/// processus fils (c'est ce que fait `kynovarad` au démarrage).
pub fn self_test() -> bool {
    build_vm(b"test key 000")
        .and_then(|vm| hash_with(&vm, b"This is a test"))
        .is_ok_and(|h| h == Hash256::from_hex_const("639183aae1bf4c9a35884cb46b09cad9175f04efd7684e7262a0ac1c2f0b4e3f"))
}

/// Clé RandomX du bloc de genèse, qui n'a pas d'ancêtre : l'empreinte nulle,
/// qui est aussi son « bloc parent » (règle de consensus).
pub const GENESIS_RANDOMX_KEY: Hash256 = Hash256::ZERO;

/// Nombre de clés dont le fil de vérification garde le cache (256 Mio
/// chacun) : la clé courante et la précédente, utile au changement de clé et
/// pour les branches concurrentes.
const VERIFIER_KEYS: usize = 2;

/// Erreur interne de RandomX (mémoire insuffisante, fil arrêté…). Ce n'est
/// jamais la faute du bloc examiné : un bloc dont la preuve n'a pas pu être
/// calculée n'est ni accepté ni déclaré invalide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PowError(pub String);

impl fmt::Display for PowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RandomX indisponible : {}", self.0)
    }
}

impl std::error::Error for PowError {}

/// Calculateur RandomX pour une clé : un cache de 256 Mio et une machine
/// virtuelle. Il ne peut pas changer de fil d'exécution : chaque fil crée le
/// sien et le garde tant que la clé ne change pas.
pub struct RandomXHasher {
    key: Hash256,
    vm: RandomXVM,
}

impl fmt::Debug for RandomXHasher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RandomXHasher").field("key", &self.key).finish()
    }
}

impl RandomXHasher {
    /// Prépare le cache de la clé `key` (environ un tiers de seconde, 256 Mio).
    pub fn new(key: &Hash256) -> Result<Self, PowError> {
        Ok(RandomXHasher { key: *key, vm: build_vm(key.as_bytes())? })
    }

    /// Clé de ce calculateur.
    pub fn key(&self) -> &Hash256 {
        &self.key
    }

    /// Empreinte RandomX de `input` (non vide) avec la clé de ce calculateur.
    pub fn hash(&self, input: &[u8]) -> Result<Hash256, PowError> {
        hash_with(&self.vm, input)
    }
}

/// Machine virtuelle en mode léger pour une clé quelconque (non vide), avec
/// le JIT s'il est permis ([`jit_allowed`]).
fn build_vm(key: &[u8]) -> Result<RandomXVM, PowError> {
    build_vm_with(key, jit_allowed())
}

fn build_vm_with(key: &[u8], jit: bool) -> Result<RandomXVM, PowError> {
    let mut flags = RandomXFlag::get_recommended_flags();
    if !jit {
        flags.remove(RandomXFlag::FLAG_JIT);
    }
    // Si le cache ne peut pas être créé avec le JIT, l'interpréteur donne
    // exactement les mêmes empreintes, plus lentement. Attention : un refus de
    // rendre la mémoire exécutable n'est PAS signalé ici par la bibliothèque
    // (voir `self_test`).
    let (cache, flags) = match RandomXCache::new(flags, key) {
        Ok(cache) => (cache, flags),
        Err(_) => {
            let mut fallback = flags;
            fallback.remove(RandomXFlag::FLAG_JIT);
            (RandomXCache::new(fallback, key).map_err(|e| PowError(e.to_string()))?, fallback)
        }
    };
    RandomXVM::new(flags, Some(cache), None).map_err(|e| PowError(e.to_string()))
}

fn hash_with(vm: &RandomXVM, input: &[u8]) -> Result<Hash256, PowError> {
    let out = vm.calculate_hash(input).map_err(|e| PowError(e.to_string()))?;
    let bytes: [u8; 32] =
        out.as_slice().try_into().map_err(|_| PowError(format!("empreinte de {} octets au lieu de 32", out.len())))?;
    Ok(Hash256(bytes))
}

/// Demande adressée au fil de vérification.
struct Job {
    key: Hash256,
    input: Vec<u8>,
    reply: mpsc::Sender<Result<Hash256, PowError>>,
}

/// Fil de vérification partagé, démarré à la première demande (None : il n'a
/// pas pu démarrer).
static VERIFIER: OnceLock<Option<mpsc::Sender<Job>>> = OnceLock::new();

/// Empreinte RandomX de `input` avec la clé `key`, calculée par le fil de
/// vérification partagé, qui garde les caches des dernières clés. Sert à
/// vérifier les blocs ; les mineurs ont leur propre [`RandomXHasher`].
pub fn randomx_hash(key: &Hash256, input: &[u8]) -> Result<Hash256, PowError> {
    let stopped = || PowError("le fil de vérification RandomX est arrêté".into());
    let verifier = VERIFIER
        .get_or_init(start_verifier)
        .as_ref()
        .ok_or_else(|| PowError("impossible de démarrer le fil de vérification RandomX".into()))?;
    let (reply, answer) = mpsc::channel();
    verifier.send(Job { key: *key, input: input.to_vec(), reply }).map_err(|_| stopped())?;
    answer.recv().map_err(|_| stopped())?
}

/// La preuve RandomX de l'en-tête est-elle satisfaite (empreinte ≤ cible)
/// avec la clé `key` ?
pub fn randomx_meets_target(header: &BlockHeader, key: &Hash256) -> Result<bool, PowError> {
    let hash = randomx_hash(key, &header.to_array())?;
    Ok(hash_meets_target(&hash, &header.target))
}

fn start_verifier() -> Option<mpsc::Sender<Job>> {
    let (sender, jobs) = mpsc::channel::<Job>();
    std::thread::Builder::new().name("randomx".into()).spawn(move || serve(&jobs)).ok()?;
    Some(sender)
}

/// Boucle du fil de vérification : une demande à la fois.
fn serve(jobs: &mpsc::Receiver<Job>) {
    // Calculateurs des dernières clés, le plus récemment utilisé en dernier.
    let mut hashers: Vec<RandomXHasher> = Vec::with_capacity(VERIFIER_KEYS);
    while let Ok(job) = jobs.recv() {
        let result = match hashers.iter().position(|h| h.key == job.key) {
            Some(i) => {
                let hasher = hashers.remove(i);
                hashers.push(hasher);
                Ok(())
            }
            None => {
                // Libérer d'abord le cache le plus ancien (256 Mio).
                if hashers.len() >= VERIFIER_KEYS {
                    hashers.remove(0);
                }
                RandomXHasher::new(&job.key).map(|hasher| hashers.push(hasher))
            }
        };
        let result = result.and_then(|()| match hashers.last() {
            Some(hasher) => hasher.hash(&job.input),
            None => Err(PowError("aucun calculateur RandomX".into())),
        });
        // Le demandeur a pu abandonner entre-temps : rien à faire.
        let _ = job.reply.send(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hex;
    use crate::u256::U256;

    #[test]
    fn official_test_vectors() {
        // Vecteurs du dépôt tevador/RandomX (src/tests/tests.cpp) : ce sont
        // ceux de Monero, ce qui prouve que la bibliothèque compilée calcule
        // le vrai RandomX.
        let vm = build_vm(b"test key 000").unwrap();
        let cases: [(&[u8], &str); 3] = [
            (b"This is a test", "639183aae1bf4c9a35884cb46b09cad9175f04efd7684e7262a0ac1c2f0b4e3f"),
            (b"Lorem ipsum dolor sit amet", "300a0adb47603dedb42228ccb2b211104f4da45af709cd7547cd049e9489c969"),
            (
                b"sed do eiusmod tempor incididunt ut labore et dolore magna aliqua",
                "c36d4ed4191e617309867ed66a443be4075014e2b061bcdaf9ce7b721d2b77a8",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(hex::encode(&hash_with(&vm, input).unwrap().0), expected);
        }
        let vm = build_vm(b"test key 001").unwrap();
        assert_eq!(
            hex::encode(
                &hash_with(&vm, b"sed do eiusmod tempor incididunt ut labore et dolore magna aliqua").unwrap().0
            ),
            "e9ff4503201c0c2cca26d285c93ae883f9b1d30c9eb240b820756f2d5a7905fc"
        );
    }

    #[test]
    fn interpreter_and_jit_give_the_same_hashes() {
        // Constat V-2 : sans JIT (système qui refuse la mémoire exécutable),
        // le nœud passe à l'interpréteur ; les empreintes doivent être les
        // mêmes, sinon il refuserait des blocs valides.
        let jit = build_vm_with(b"cle kynovara", true).unwrap();
        let interpreter = build_vm_with(b"cle kynovara", false).unwrap();
        for i in 0..3u8 {
            assert_eq!(hash_with(&jit, &[i; 116]).unwrap(), hash_with(&interpreter, &[i; 116]).unwrap());
        }
        // Dans un environnement ordinaire, l'essai de démarrage réussit.
        assert!(self_test());
    }

    #[test]
    fn verifier_matches_a_private_hasher_and_survives_key_changes() {
        let keys = [Hash256([1; 32]), Hash256([2; 32]), Hash256([3; 32])];
        let hashers: Vec<RandomXHasher> = keys.iter().map(|k| RandomXHasher::new(k).unwrap()).collect();
        // Trois clés pour deux places : le fil doit recréer les caches oubliés
        // sans jamais se tromper de clé.
        for round in 0..2u8 {
            for (key, hasher) in keys.iter().zip(&hashers) {
                let input = [round; 116];
                assert_eq!(randomx_hash(key, &input).unwrap(), hasher.hash(&input).unwrap());
            }
        }
        // La clé change l'empreinte.
        assert_ne!(hashers[0].hash(b"kynovara").unwrap(), hashers[1].hash(b"kynovara").unwrap());
    }

    #[test]
    fn verifier_serves_concurrent_callers() {
        let key = Hash256([7; 32]);
        let reference = RandomXHasher::new(&key).unwrap();
        let expected: Vec<Hash256> = (0..8u8).map(|i| reference.hash(&[i; 116]).unwrap()).collect();
        std::thread::scope(|scope| {
            for (i, want) in expected.iter().enumerate() {
                scope.spawn(move || assert_eq!(randomx_hash(&key, &[i as u8; 116]).unwrap(), *want));
            }
        });
    }

    #[test]
    fn header_check_compares_hash_and_target() {
        let key = Hash256([9; 32]);
        let mut header = BlockHeader {
            version: 1,
            prev_hash: Hash256([4; 32]),
            merkle_root: Hash256([5; 32]),
            timestamp: 1_800_000_000,
            target: U256::ZERO,
            nonce: 0,
        };
        // Cible nulle : impossible ; cible maximale : toujours satisfaite.
        assert!(!randomx_meets_target(&header, &key).unwrap());
        header.target = U256::MAX;
        assert!(randomx_meets_target(&header, &key).unwrap());
        // L'empreinte porte sur tout l'en-tête : le nonce la change.
        let a = randomx_hash(&key, &header.to_array()).unwrap();
        header.nonce = 1;
        assert_ne!(a, randomx_hash(&key, &header.to_array()).unwrap());
    }
}
