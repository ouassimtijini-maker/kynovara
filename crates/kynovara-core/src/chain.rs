//! État de la chaîne : index de tous les blocs connus, chaîne active,
//! ensemble UTXO et réorganisations.
//!
//! Règle de sélection (consensus) : la chaîne active est la chaîne **valide**
//! qui a le plus grand travail cumulé. À travail égal, la première vue est
//! conservée.
//!
//! Déroulement de [`Chain::process_block`] :
//! 1. vérifications sans contexte (preuve de travail, Merkle, format) ;
//! 2. si le parent est inconnu : le bloc est mis de côté (orphelin) ;
//! 3. vérifications de l'en-tête dans son contexte (cible exacte, horodatage) ;
//! 4. stockage et ajout à l'index ;
//! 5. si une branche a désormais plus de travail que la chaîne active, on
//!    s'y déplace (réorganisation). Chaque bloc y est validé complètement ;
//!    un bloc invalide est marqué (avec tous ses descendants) et on revient à
//!    la meilleure chaîne valide.
//!
//! Protection du disque (politique locale, pas une règle de consensus) : un
//! bloc d'une branche concurrente partant de **loin dans le passé** (plus de
//! `DEFAULT_SIDE_BRANCH_MARGIN` blocs de travail sous le sommet) et qui ne
//! dépasse pas encore la chaîne active n'est pas écrit sur le disque : il est
//! gardé en mémoire, dans une réserve bornée. Dès que sa branche revient dans
//! la course, elle est écrite sur le disque (ancêtres d'abord), puis adoptée si
//! elle devient la plus lourde. Les branches proches du sommet (blocs
//! concurrents ordinaires, réorganisations courantes) sont écrites tout de
//! suite : les produire coûte une vraie puissance de calcul. Sans cette
//! règle, un attaquant pourrait remplir le disque de longues branches à faible
//! difficulté partant de loin dans le passé. Quand la réserve est pleine, les
//! feuilles invalides, puis celles de plus faible travail, sont oubliées en
//! premier : une vraie branche concurrente passe avant des branches bon
//! marché. Invariant : un bloc écrit sur le disque a tous ses ancêtres sur le
//! disque.

use crate::block::{Block, BlockHeader};
use crate::difficulty;
use crate::genesis::genesis_block;
use crate::hash::Hash256;
use crate::params::{ConsensusParams, Network};
use crate::u256::{U256, work_from_target};
use crate::utxo::{BlockUndo, UtxoSet};
use crate::validation::{BlockError, check_block_structure, connect_block, disconnect_block};
use std::cmp::Reverse;
use std::collections::{HashMap, VecDeque};
use std::fmt;

/// Nombre maximal d'empreintes dans un localisateur de blocs.
pub const MAX_LOCATOR: usize = 64;
/// Nombre de blocs récents dont on garde les données d'annulation en mémoire.
/// Une réorganisation plus profonde reconstruit l'état depuis la genèse. (Ces
/// données vivent en mémoire : la profondeur borne la mémoire consommée par
/// des blocs pleins.)
pub const DEFAULT_UNDO_DEPTH: u64 = 1_000;
/// Une branche concurrente qui part de moins de `DEFAULT_SIDE_BRANCH_MARGIN`
/// blocs (en travail) sous le sommet est écrite sur le disque même si elle est
/// plus légère : elle peut gagner, et la produire coûte une vraie puissance de
/// calcul. Seules les branches plus profondes restent en mémoire.
pub const DEFAULT_SIDE_BRANCH_MARGIN: u64 = 1_000;
const MAX_ORPHANS: usize = 64;
const MAX_ORPHAN_BYTES: usize = 16_000_000;
/// Blocs de branches secondaires gardés en mémoire au plus (voir l'en-tête du
/// module) : en nombre et en taille encodée.
pub const DEFAULT_MAX_PARKED_BLOCKS: usize = 100_000;
pub const DEFAULT_MAX_PARKED_BYTES: usize = 64 * 1024 * 1024;

/// Stockage des blocs complets (fichiers sur disque pour le nœud, mémoire
/// pour les tests).
pub trait BlockStore {
    fn put(&mut self, hash: &Hash256, block: &Block) -> Result<(), String>;
    fn get(&self, hash: &Hash256) -> Result<Option<Block>, String>;
    fn contains(&self, hash: &Hash256) -> bool;
}

/// Stockage en mémoire (tests).
#[derive(Debug, Default)]
pub struct MemoryStore {
    blocks: HashMap<Hash256, Block>,
}

impl BlockStore for MemoryStore {
    fn put(&mut self, hash: &Hash256, block: &Block) -> Result<(), String> {
        self.blocks.entry(*hash).or_insert_with(|| block.clone());
        Ok(())
    }

    fn get(&self, hash: &Hash256) -> Result<Option<Block>, String> {
        Ok(self.blocks.get(hash).cloned())
    }

    fn contains(&self, hash: &Hash256) -> bool {
        self.blocks.contains_key(hash)
    }
}

/// État de validation d'un bloc indexé.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockStatus {
    /// En-tête valide et données stockées, pas encore connecté.
    Pending,
    /// Connecté avec succès (entièrement validé) au moins une fois.
    Valid,
    /// Invalide (ou descendant d'un bloc invalide).
    Invalid,
}

/// Entrée de l'index des blocs.
#[derive(Clone, Debug)]
pub struct IndexEntry {
    pub hash: Hash256,
    pub header: BlockHeader,
    pub height: u64,
    /// Travail cumulé depuis la genèse (inclus).
    pub chain_work: U256,
    pub status: BlockStatus,
    pub size: usize,
    pub tx_count: usize,
    /// Ordre d'arrivée (départage à travail égal).
    seq: u64,
}

/// Erreur d'initialisation de la chaîne.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainError {
    NotLaunched(Network),
    GenesisMismatch { expected: Hash256, computed: Hash256 },
    Storage(String),
}

impl fmt::Display for ChainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChainError::NotLaunched(n) => write!(
                f,
                "le réseau {n} n'est pas encore lancé : son bloc de genèse n'existe pas (voir docs/LANCEMENT.md)"
            ),
            ChainError::GenesisMismatch { expected, computed } => {
                write!(f, "bloc de genèse incorrect : attendu {expected}, calculé {computed}")
            }
            ChainError::Storage(e) => write!(f, "erreur de stockage : {e}"),
        }
    }
}

impl std::error::Error for ChainError {}

/// Changement de la chaîne active.
#[derive(Debug, Default)]
pub struct TipChange {
    /// Blocs retirés de la chaîne active (du plus haut au plus bas).
    pub disconnected: Vec<Block>,
    /// Blocs ajoutés à la chaîne active (par hauteur croissante).
    pub connected: Vec<Block>,
    /// Changement trop grand pour être gardé entièrement en mémoire : seuls
    /// les blocs les plus hauts de chaque liste y figurent. Les utilisateurs
    /// (mempool) doivent alors tout revérifier contre le nouvel état.
    pub truncated: bool,
}

/// Taille maximale du compte rendu d'un changement de chaîne (une
/// réorganisation très profonde de blocs pleins ne doit pas épuiser la
/// mémoire, ni pendant la bascule, ni au redémarrage qui la rejoue).
pub const MAX_TIP_CHANGE_BLOCKS: usize = 1_000;
pub const MAX_TIP_CHANGE_BYTES: usize = 64 * 1024 * 1024;

/// Résultat de l'application d'un point de contrôle.
#[derive(Debug)]
pub enum CheckpointOutcome {
    /// Déjà connu, ou plus ancien et cohérent avec le point courant.
    AlreadyKnown,
    /// Bloc pas encore connu : le demander, puis réessayer.
    UnknownBlock,
    /// Appliqué ; la chaîne active a pu changer.
    Applied { tip_change: Option<TipChange>, invalid: Vec<(Hash256, BlockError)> },
    /// Contradiction avec le point courant, ou bloc invalide : alerte.
    Conflict { reason: String, tip_change: Option<TipChange> },
}

/// Réunit deux changements successifs (compte rendu marqué tronqué : la
/// mempool revérifie alors tout).
fn merge_tip_changes(a: Option<TipChange>, b: Option<TipChange>) -> Option<TipChange> {
    match (a, b) {
        (None, x) | (x, None) => x,
        (Some(mut a), Some(b)) => {
            a.disconnected.extend(b.disconnected);
            a.connected.extend(b.connected);
            a.truncated = true;
            Some(a)
        }
    }
}

/// Résultat du traitement d'un bloc.
#[derive(Debug)]
pub enum ProcessOutcome {
    /// Bloc déjà connu.
    AlreadyKnown,
    /// Parent inconnu : bloc mis de côté en attendant son parent.
    Orphan { missing_parent: Hash256 },
    /// Bloc accepté dans l'index. `tip_change` décrit l'éventuel changement de
    /// chaîne active ; `invalid` liste les blocs trouvés invalides au moment
    /// de les connecter (y compris, éventuellement, ce bloc-ci).
    Accepted { tip_change: Option<TipChange>, invalid: Vec<(Hash256, BlockError)> },
}

enum SwitchError {
    Invalid(Hash256, BlockError),
    Fatal(String),
}

#[derive(Debug, Default)]
struct OrphanPool {
    blocks: HashMap<Hash256, Block>,
    by_parent: HashMap<Hash256, Vec<Hash256>>,
    order: VecDeque<Hash256>,
    bytes: usize,
}

impl OrphanPool {
    fn contains(&self, hash: &Hash256) -> bool {
        self.blocks.contains_key(hash)
    }

    fn insert(&mut self, hash: Hash256, block: Block) {
        if self.blocks.contains_key(&hash) {
            return;
        }
        let size = block.size();
        while !self.order.is_empty() && (self.blocks.len() >= MAX_ORPHANS || self.bytes + size > MAX_ORPHAN_BYTES) {
            if let Some(oldest) = self.order.pop_front() {
                self.remove(&oldest);
            }
        }
        self.by_parent.entry(block.header.prev_hash).or_default().push(hash);
        self.order.push_back(hash);
        self.bytes += size;
        self.blocks.insert(hash, block);
    }

    fn remove(&mut self, hash: &Hash256) -> Option<Block> {
        let block = self.blocks.remove(hash)?;
        self.bytes -= block.size();
        self.order.retain(|h| h != hash);
        if let Some(siblings) = self.by_parent.get_mut(&block.header.prev_hash) {
            siblings.retain(|h| h != hash);
            if siblings.is_empty() {
                self.by_parent.remove(&block.header.prev_hash);
            }
        }
        Some(block)
    }

    fn take_children(&mut self, parent: &Hash256) -> Vec<(Hash256, Block)> {
        let hashes = self.by_parent.get(parent).cloned().unwrap_or_default();
        hashes.into_iter().filter_map(|h| self.remove(&h).map(|b| (h, b))).collect()
    }
}

/// La chaîne.
pub struct Chain<S: BlockStore> {
    params: &'static ConsensusParams,
    genesis_hash: Hash256,
    store: S,
    index: HashMap<Hash256, IndexEntry>,
    children: HashMap<Hash256, Vec<Hash256>>,
    active: Vec<Hash256>,
    utxo: UtxoSet,
    undo: HashMap<Hash256, BlockUndo>,
    undo_depth: u64,
    tx_heights: HashMap<Hash256, u64>,
    orphans: OrphanPool,
    /// Blocs indexés mais gardés en mémoire seulement (branches secondaires
    /// qui ne dépassent pas la chaîne active).
    parked: HashMap<Hash256, Block>,
    parked_bytes: usize,
    parked_limits: (usize, usize),
    tip_change_limits: (usize, usize),
    /// Profondeur (en blocs) en deçà de laquelle une branche concurrente est
    /// écrite sur le disque même si elle est plus légère (voir l'en-tête).
    side_branch_margin: u64,
    /// Erreur de stockage grave : la chaîne refuse alors tout nouveau bloc.
    fatal: Option<String>,
    /// Dernier point de contrôle signé appliqué (hauteur, bloc) : toute
    /// branche qui ne le contient pas est écartée.
    checkpoint: Option<(u64, Hash256)>,
    /// Chaîne gelée (alerte : points de contrôle contradictoires) : le sommet
    /// peut seulement être prolongé, jamais remplacé.
    frozen: bool,
    best_candidate: Hash256,
    next_seq: u64,
}

impl<S: BlockStore> fmt::Debug for Chain<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Chain")
            .field("network", &self.params.network)
            .field("height", &self.height())
            .field("tip", &self.tip_hash())
            .finish()
    }
}

impl<S: BlockStore> Chain<S> {
    /// Ouvre la chaîne : vérifie la genèse et l'ajoute au stockage si besoin.
    pub fn new(params: &'static ConsensusParams, mut store: S) -> Result<Self, ChainError> {
        let expected = params.genesis.hash.ok_or(ChainError::NotLaunched(params.network))?;
        let genesis = genesis_block(params);
        let hash = genesis.header.hash();
        if hash != expected {
            return Err(ChainError::GenesisMismatch { expected, computed: hash });
        }
        if !store.contains(&hash) {
            store.put(&hash, &genesis).map_err(ChainError::Storage)?;
        }
        let entry = IndexEntry {
            hash,
            header: genesis.header,
            height: 0,
            chain_work: work_from_target(genesis.header.target),
            status: BlockStatus::Valid,
            size: genesis.size(),
            tx_count: genesis.transactions.len(),
            seq: 0,
        };
        let mut tx_heights = HashMap::new();
        for tx in &genesis.transactions {
            tx_heights.insert(tx.txid(), 0);
        }
        Ok(Chain {
            params,
            genesis_hash: hash,
            store,
            index: HashMap::from([(hash, entry)]),
            children: HashMap::new(),
            active: vec![hash],
            utxo: UtxoSet::new(),
            undo: HashMap::new(),
            undo_depth: DEFAULT_UNDO_DEPTH,
            tx_heights,
            orphans: OrphanPool::default(),
            parked: HashMap::new(),
            parked_bytes: 0,
            parked_limits: (DEFAULT_MAX_PARKED_BLOCKS, DEFAULT_MAX_PARKED_BYTES),
            tip_change_limits: (MAX_TIP_CHANGE_BLOCKS, MAX_TIP_CHANGE_BYTES),
            side_branch_margin: DEFAULT_SIDE_BRANCH_MARGIN,
            fatal: None,
            checkpoint: None,
            frozen: false,
            best_candidate: hash,
            next_seq: 1,
        })
    }

    /// Modifie la profondeur des données d'annulation conservées (tests).
    pub fn set_undo_depth(&mut self, depth: u64) {
        self.undo_depth = depth.max(1);
    }

    /// Modifie les limites de la réserve de blocs en mémoire (tests).
    pub fn set_parked_limits(&mut self, max_blocks: usize, max_bytes: usize) {
        self.parked_limits = (max_blocks, max_bytes);
        self.enforce_parked_limits();
    }

    /// Modifie les limites du compte rendu des changements de chaîne (tests).
    pub fn set_tip_change_limits(&mut self, max_blocks: usize, max_bytes: usize) {
        self.tip_change_limits = (max_blocks, max_bytes);
    }

    /// Modifie la marge (en blocs) des branches concurrentes écrites sur le
    /// disque même si elles sont plus légères (0 : seulement si elles
    /// dépassent la chaîne active).
    pub fn set_side_branch_margin(&mut self, blocks: u64) {
        self.side_branch_margin = blocks;
    }

    /// Erreur de stockage grave rencontrée (la chaîne n'accepte alors plus
    /// aucun bloc : le nœud doit s'arrêter et être relancé).
    pub fn fatal_error(&self) -> Option<&str> {
        self.fatal.as_deref()
    }

    /// Nombre et taille encodée des blocs gardés en mémoire seulement.
    /// Dernier point de contrôle appliqué (hauteur, bloc).
    pub fn checkpoint(&self) -> Option<(u64, Hash256)> {
        self.checkpoint
    }

    /// Gèle (ou dégèle) la chaîne : gelée, elle ne fait plus que prolonger
    /// son sommet (alerte de points de contrôle contradictoires).
    pub fn set_frozen(&mut self, frozen: bool) {
        self.frozen = frozen;
        if !frozen {
            self.recompute_best_candidate();
        }
    }

    pub fn is_frozen(&self) -> bool {
        self.frozen
    }

    /// Le bloc indexé `hash` est-il compatible avec le point de contrôle (il
    /// le contient, ou il en est un ancêtre) ?
    pub fn respects_checkpoint(&self, hash: &Hash256) -> bool {
        let Some((cp_height, cp_hash)) = self.checkpoint else {
            return true;
        };
        let Some(entry) = self.index.get(hash) else {
            return false;
        };
        if entry.height >= cp_height {
            self.ancestor(hash, cp_height) == Some(cp_hash)
        } else {
            self.ancestor(&cp_hash, entry.height) == Some(*hash)
        }
    }

    /// Applique un point de contrôle **déjà vérifié** (signature, délégation)
    /// au bloc `hash` de hauteur `height`. Avec `force` (clé maîtresse), il
    /// remplace le point courant même s'il le contredit.
    pub fn apply_checkpoint(&mut self, height: u64, hash: Hash256, force: bool) -> CheckpointOutcome {
        // Sous le point courant, l'historique est connu : un autre bloc à
        // cette hauteur est une contradiction, même si on ne le connaît pas.
        if let Some((cur_height, cur_hash)) = self.checkpoint
            && height <= cur_height
            && !force
        {
            return match self.ancestor(&cur_hash, height) {
                Some(ours) if ours == hash => CheckpointOutcome::AlreadyKnown,
                _ => CheckpointOutcome::Conflict {
                    reason: format!("contredit le point de contrôle de la hauteur {cur_height} ({cur_hash})"),
                    tip_change: None,
                },
            };
        }
        let Some(entry) = self.index.get(&hash) else {
            return CheckpointOutcome::UnknownBlock;
        };
        if entry.height != height {
            return CheckpointOutcome::Conflict {
                reason: format!("le bloc {hash} n'est pas à la hauteur {height}"),
                tip_change: None,
            };
        }
        if entry.status == BlockStatus::Invalid {
            return CheckpointOutcome::Conflict { reason: format!("le bloc {hash} est invalide"), tip_change: None };
        }
        if let Some((cur_height, cur_hash)) = self.checkpoint {
            if (cur_height, cur_hash) == (height, hash) {
                return CheckpointOutcome::AlreadyKnown;
            }
            let consistent = if height <= cur_height {
                self.ancestor(&cur_hash, height) == Some(hash)
            } else {
                self.ancestor(&hash, cur_height) == Some(cur_hash)
            };
            if consistent && height <= cur_height {
                return CheckpointOutcome::AlreadyKnown;
            }
            if !consistent && !force {
                return CheckpointOutcome::Conflict {
                    reason: format!("contredit le point de contrôle de la hauteur {cur_height} ({cur_hash})"),
                    tip_change: None,
                };
            }
        }
        let previous = self.checkpoint;
        self.checkpoint = Some((height, hash));
        self.recompute_best_candidate();
        let (first, invalid) = self.activate_best_chain();
        if self.index.get(&hash).is_some_and(|e| e.status == BlockStatus::Invalid) {
            // Le point désignait un bloc invalide (ou qui descend d'un bloc
            // invalide) : retour au point précédent.
            self.checkpoint = previous;
            self.recompute_best_candidate();
            let (second, _) = self.activate_best_chain();
            return CheckpointOutcome::Conflict {
                reason: format!("le bloc {hash} s'est révélé invalide"),
                tip_change: merge_tip_changes(first, second),
            };
        }
        CheckpointOutcome::Applied { tip_change: first, invalid }
    }

    pub fn parked_stats(&self) -> (usize, usize) {
        (self.parked.len(), self.parked_bytes)
    }

    /// Le bloc est-il indexé mais gardé en mémoire seulement ?
    pub fn is_parked(&self, hash: &Hash256) -> bool {
        self.parked.contains_key(hash)
    }

    pub fn params(&self) -> &'static ConsensusParams {
        self.params
    }

    pub fn genesis_hash(&self) -> Hash256 {
        self.genesis_hash
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    pub fn utxo(&self) -> &UtxoSet {
        &self.utxo
    }

    /// Hauteur de la chaîne active (0 = seulement la genèse).
    pub fn height(&self) -> u64 {
        self.active.len() as u64 - 1
    }

    pub fn tip_hash(&self) -> Hash256 {
        self.active[self.active.len() - 1]
    }

    /// Entrée d'index du sommet de la chaîne active.
    pub fn tip(&self) -> &IndexEntry {
        &self.index[&self.tip_hash()]
    }

    pub fn entry(&self, hash: &Hash256) -> Option<&IndexEntry> {
        self.index.get(hash)
    }

    pub fn block_hash_at(&self, height: u64) -> Option<Hash256> {
        self.active.get(usize::try_from(height).ok()?).copied()
    }

    /// Le bloc fait-il partie de la chaîne active ?
    pub fn is_active(&self, hash: &Hash256) -> bool {
        self.index.get(hash).is_some_and(|e| self.active.get(e.height as usize) == Some(hash))
    }

    /// Bloc connu (indexé ou orphelin) ?
    pub fn contains(&self, hash: &Hash256) -> bool {
        self.index.contains_key(hash) || self.orphans.contains(hash)
    }

    pub fn orphan_count(&self) -> usize {
        self.orphans.blocks.len()
    }

    pub fn get_block(&self, hash: &Hash256) -> Result<Option<Block>, String> {
        if !self.index.contains_key(hash) {
            return Ok(None);
        }
        if let Some(block) = self.parked.get(hash) {
            return Ok(Some(block.clone()));
        }
        self.store.get(hash)
    }

    /// Hauteur du bloc actif contenant la transaction `txid`.
    pub fn tx_height(&self, txid: &Hash256) -> Option<u64> {
        self.tx_heights.get(txid).copied()
    }

    /// Ancêtre de `hash` à la hauteur `height`.
    pub fn ancestor(&self, hash: &Hash256, height: u64) -> Option<Hash256> {
        let mut entry = self.index.get(hash)?;
        if height > entry.height {
            return None;
        }
        loop {
            if self.is_active(&entry.hash) {
                return self.block_hash_at(height);
            }
            if entry.height == height {
                return Some(entry.hash);
            }
            entry = self.index.get(&entry.header.prev_hash)?;
        }
    }

    fn difficulty_window(&self, parent: &IndexEntry) -> Vec<(u64, U256)> {
        let n = self.params.lwma_window;
        if self.params.no_retarget || parent.height < n {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(n as usize + 1);
        let mut cur = parent;
        loop {
            out.push((cur.header.timestamp, cur.header.target));
            if out.len() as u64 == n + 1 {
                break;
            }
            match self.index.get(&cur.header.prev_hash) {
                Some(p) => cur = p,
                None => break,
            }
        }
        out.reverse();
        out
    }

    /// Cible exigée pour un enfant du bloc `parent_hash`.
    pub fn next_target_after(&self, parent_hash: &Hash256) -> Option<U256> {
        let parent = self.index.get(parent_hash)?;
        Some(difficulty::next_target(self.params, parent.height, &self.difficulty_window(parent)))
    }

    /// Traite un bloc reçu (du réseau ou du mineur local).
    pub fn process_block(&mut self, block: Block, now: u64) -> Result<ProcessOutcome, BlockError> {
        if let Some(message) = &self.fatal {
            return Err(BlockError::Storage(format!("chaîne arrêtée après une erreur de stockage : {message}")));
        }
        let hash = block.header.hash();
        if let Some(entry) = self.index.get(&hash) {
            return match entry.status {
                BlockStatus::Invalid => Err(BlockError::KnownInvalid),
                _ => Ok(ProcessOutcome::AlreadyKnown),
            };
        }
        if self.orphans.contains(&hash) {
            return Ok(ProcessOutcome::AlreadyKnown);
        }
        check_block_structure(&block, self.params)?;
        if !self.index.contains_key(&block.header.prev_hash) {
            let missing_parent = block.header.prev_hash;
            self.orphans.insert(hash, block);
            return Ok(ProcessOutcome::Orphan { missing_parent });
        }
        self.accept_with_parent(hash, block, now)?;

        // Les orphelins qui attendaient ce bloc peuvent maintenant être indexés.
        let mut queue = vec![hash];
        while let Some(parent) = queue.pop() {
            for (child_hash, child) in self.orphans.take_children(&parent) {
                if self.accept_with_parent(child_hash, child, now).is_ok() {
                    queue.push(child_hash);
                }
            }
        }
        self.enforce_parked_limits();
        self.adopt_release_checkpoint();

        let (tip_change, invalid) = self.activate_best_chain();
        Ok(ProcessOutcome::Accepted { tip_change, invalid })
    }

    /// Vérifications contextuelles de l'en-tête, puis stockage (disque ou
    /// réserve en mémoire) et indexation.
    fn accept_with_parent(&mut self, hash: Hash256, block: Block, now: u64) -> Result<(), BlockError> {
        let parent = self
            .index
            .get(&block.header.prev_hash)
            .cloned()
            .ok_or(BlockError::Storage("parent absent de l'index".into()))?;
        if parent.status == BlockStatus::Invalid {
            return Err(BlockError::InvalidAncestor(parent.hash));
        }
        // Un nouveau bloc ne peut pas être un ancêtre du point de contrôle :
        // il doit le prolonger. Les branches concurrentes plus anciennes sont
        // refusées d'emblée (ni stockées, ni indexées).
        if let Some((cp_height, cp_hash)) = self.checkpoint
            && (parent.height < cp_height || self.ancestor(&parent.hash, cp_height) != Some(cp_hash))
        {
            return Err(BlockError::ConflictsWithCheckpoint { height: cp_height });
        }
        let height = parent.height + 1;
        // Point inscrit dans la version : à cette hauteur, un seul bloc admis.
        if self.params.release_checkpoint_at(height).is_some_and(|b| b != hash) {
            return Err(BlockError::ConflictsWithCheckpoint { height });
        }
        let expected = difficulty::next_target(self.params, parent.height, &self.difficulty_window(&parent));
        if block.header.target != expected {
            return Err(BlockError::BadTarget { expected, found: block.header.target });
        }
        if block.header.timestamp <= parent.header.timestamp {
            return Err(BlockError::TimestampNotIncreasing {
                parent: parent.header.timestamp,
                found: block.header.timestamp,
            });
        }
        let max_time = now.saturating_add(self.params.max_future_block_time);
        if block.header.timestamp > max_time {
            return Err(BlockError::TimestampTooFarInFuture { max: max_time, found: block.header.timestamp });
        }
        let coinbase_height = block.coinbase().and_then(|c| c.coinbase_height());
        if coinbase_height != Some(height) {
            return Err(BlockError::CoinbaseHeightMismatch { expected: height, found: coinbase_height });
        }
        // Période de rodage : refusé avant d'être stocké.
        crate::validation::check_transfers_allowed(&block, height, self.params)?;

        let chain_work = parent.chain_work.saturating_add(work_from_target(block.header.target));
        let size = block.size();
        let entry = IndexEntry {
            hash,
            header: block.header,
            height,
            chain_work,
            status: BlockStatus::Pending,
            size,
            tx_count: block.transactions.len(),
            seq: self.next_seq,
        };
        // Un bloc écrit sur le disque si sa branche est « en course » : plus de
        // travail que notre chaîne n'en avait `side_branch_margin` blocs sous le
        // sommet (ce qui inclut tout bloc qui dépasse la chaîne active). Sinon,
        // il reste en mémoire. Un bloc déjà sur le disque (relecture au
        // démarrage) y reste. L'écriture se fait après les ancêtres encore en
        // mémoire.
        let on_disk = self.store.contains(&hash);
        if on_disk || chain_work > self.disk_threshold() {
            self.flush_parked(&parent.hash)?;
            if !on_disk {
                self.store.put(&hash, &block).map_err(BlockError::Storage)?;
            }
        } else {
            self.parked_bytes += size;
            self.parked.insert(hash, block);
        }
        self.next_seq += 1;
        self.children.entry(parent.hash).or_default().push(hash);
        let best_work = self.index.get(&self.best_candidate).map_or(U256::ZERO, |e| e.chain_work);
        self.index.insert(hash, entry);
        if chain_work > best_work && self.respects_checkpoint(&hash) {
            self.best_candidate = hash;
        }
        Ok(())
    }

    /// Travail cumulé de la chaîne active `side_branch_margin` blocs sous le
    /// sommet (le sommet lui-même avec une marge nulle).
    fn disk_threshold(&self) -> U256 {
        let height = self.height().saturating_sub(self.side_branch_margin);
        self.block_hash_at(height).and_then(|h| self.index.get(&h)).map_or(U256::ZERO, |e| e.chain_work)
    }

    /// Se place sur la meilleure chaîne valide.
    fn activate_best_chain(&mut self) -> (Option<TipChange>, Vec<(Hash256, BlockError)>) {
        let mut disconnected = Vec::new();
        let mut connected = Vec::new();
        let mut invalid = Vec::new();
        loop {
            // Gelée, la chaîne ne considère que les prolongements de son sommet.
            let target = if self.frozen { self.best_extension_of_tip() } else { self.best_candidate };
            let tip = self.tip_hash();
            if target == tip {
                break;
            }
            let target_work = self.index.get(&target).map_or(U256::ZERO, |e| e.chain_work);
            // Un sommet qui ne respecte plus le point de contrôle est quitté
            // même pour une branche plus légère.
            if target_work <= self.tip().chain_work && self.respects_checkpoint(&tip) {
                break;
            }
            match self.switch_to(target, &mut disconnected, &mut connected) {
                Ok(()) => {}
                Err(SwitchError::Invalid(bad, error)) => {
                    self.mark_invalid(&bad);
                    invalid.push((bad, error));
                    self.recompute_best_candidate();
                }
                Err(SwitchError::Fatal(message)) => {
                    // Le stockage ne permet plus de suivre la chaîne : on
                    // s'arrête net plutôt que de continuer sur un état douteux.
                    self.fatal = Some(message.clone());
                    invalid.push((target, BlockError::Storage(message)));
                    break;
                }
            }
        }

        // Effet net par rapport à l'état de départ : un bloc retiré puis remis
        // (ou ajouté puis retiré) pendant les essais n'apparaît pas. Seules les
        // empreintes sont gardées pendant la bascule : les blocs ne sont
        // rechargés que pour le compte rendu, dans la limite autorisée.
        let mut delta: HashMap<Hash256, i64> = HashMap::new();
        for h in disconnected {
            *delta.entry(h).or_default() -= 1;
        }
        for h in connected {
            *delta.entry(h).or_default() += 1;
        }
        let height_of = |h: &Hash256| self.index.get(h).map_or(0, |e| e.height);
        let mut gone: Vec<(u64, Hash256)> = Vec::new();
        let mut added: Vec<(u64, Hash256)> = Vec::new();
        for (h, d) in delta {
            match d {
                d if d < 0 => gone.push((height_of(&h), h)),
                d if d > 0 => added.push((height_of(&h), h)),
                _ => {}
            }
        }
        if gone.is_empty() && added.is_empty() {
            return (None, invalid);
        }
        // Les plus hauts d'abord : en cas de dépassement, ce sont eux qui restent.
        gone.sort_by_key(|(height, _)| Reverse(*height));
        added.sort_by_key(|(height, _)| Reverse(*height));
        let mut change = TipChange::default();
        let gone_complete = self.load_for_report(&gone, &mut change.disconnected);
        let added_complete = self.load_for_report(&added, &mut change.connected);
        change.connected.reverse();
        change.truncated = !(gone_complete && added_complete);
        (Some(change), invalid)
    }

    /// Charge les blocs de `list` dans `out` sans dépasser les limites du
    /// compte rendu. Renvoie `false` si la liste a dû être tronquée.
    fn load_for_report(&self, list: &[(u64, Hash256)], out: &mut Vec<Block>) -> bool {
        let (mut blocks_left, mut bytes_left) = self.tip_change_limits;
        for (_, h) in list {
            let size = self.index.get(h).map_or(0, |e| e.size);
            if blocks_left == 0 || size > bytes_left {
                return false;
            }
            let Ok(block) = self.load(h) else {
                return false;
            };
            blocks_left -= 1;
            bytes_left -= size;
            out.push(block);
        }
        true
    }

    fn switch_to(
        &mut self,
        target: Hash256,
        disconnected: &mut Vec<Hash256>,
        connected: &mut Vec<Hash256>,
    ) -> Result<(), SwitchError> {
        // Toute la branche visée doit être sur le disque avant d'y basculer
        // (normalement déjà fait à l'arrivée du bloc qui l'a rendue plus lourde).
        self.flush_parked(&target).map_err(|e| SwitchError::Fatal(e.to_string()))?;
        let mut path = Vec::new();
        let mut cur = target;
        while !self.is_active(&cur) {
            path.push(cur);
            cur = match self.index.get(&cur) {
                Some(e) => e.header.prev_hash,
                None => return Err(SwitchError::Fatal(format!("ancêtre {cur} absent de l'index"))),
            };
        }
        let fork_height = self.index.get(&cur).map_or(0, |e| e.height);
        while self.height() > fork_height {
            let block = self.disconnect_tip().map_err(|e| SwitchError::Fatal(e.to_string()))?;
            disconnected.push(block.hash());
        }
        for hash in path.into_iter().rev() {
            match self.connect_tip(&hash) {
                Ok(_) => connected.push(hash),
                Err(BlockError::Storage(message)) => return Err(SwitchError::Fatal(message)),
                Err(error) => return Err(SwitchError::Invalid(hash, error)),
            }
        }
        Ok(())
    }

    fn load(&self, hash: &Hash256) -> Result<Block, BlockError> {
        if let Some(block) = self.parked.get(hash) {
            return Ok(block.clone());
        }
        self.store
            .get(hash)
            .map_err(BlockError::Storage)?
            .ok_or_else(|| BlockError::Storage(format!("bloc {hash} absent du stockage")))
    }

    /// Écrit sur le disque `hash` et ses ancêtres encore en mémoire, du plus
    /// ancien au plus récent (les blocs en mémoire forment toujours le haut
    /// d'une branche : la remontée s'arrête au premier bloc déjà écrit).
    fn flush_parked(&mut self, hash: &Hash256) -> Result<(), BlockError> {
        let mut path = Vec::new();
        let mut cur = *hash;
        while self.parked.contains_key(&cur) {
            path.push(cur);
            match self.index.get(&cur) {
                Some(entry) => cur = entry.header.prev_hash,
                None => break,
            }
        }
        for h in path.into_iter().rev() {
            if let Some(block) = self.parked.get(&h) {
                self.store.put(&h, block).map_err(BlockError::Storage)?;
            }
            if let Some(block) = self.parked.remove(&h) {
                self.parked_bytes = self.parked_bytes.saturating_sub(block.size());
            }
        }
        Ok(())
    }

    /// Fait respecter les limites de la réserve en mémoire en oubliant des
    /// feuilles : invalides d'abord, puis celles de plus faible travail cumulé
    /// (puis les plus récentes). Une feuille oubliée disparaît de l'index ; elle
    /// pourra être reçue à nouveau plus tard.
    fn enforce_parked_limits(&mut self) {
        let (max_blocks, max_bytes) = self.parked_limits;
        while self.parked.len() > max_blocks || self.parked_bytes > max_bytes {
            let victim = self
                .parked
                .keys()
                .filter(|h| self.children.get(*h).is_none_or(Vec::is_empty))
                .filter_map(|h| self.index.get(h))
                .min_by_key(|e| (e.status != BlockStatus::Invalid, e.chain_work, Reverse(e.seq)))
                .map(|e| e.hash);
            let Some(victim) = victim else { break };
            self.evict_parked(&victim);
        }
    }

    fn evict_parked(&mut self, hash: &Hash256) {
        if let Some(block) = self.parked.remove(hash) {
            self.parked_bytes = self.parked_bytes.saturating_sub(block.size());
        }
        if let Some(entry) = self.index.remove(hash) {
            let parent = entry.header.prev_hash;
            if let Some(siblings) = self.children.get_mut(&parent) {
                siblings.retain(|h| h != hash);
                if siblings.is_empty() {
                    self.children.remove(&parent);
                }
            }
        }
        self.children.remove(hash);
        if self.best_candidate == *hash {
            self.recompute_best_candidate();
        }
    }

    fn connect_tip(&mut self, hash: &Hash256) -> Result<Block, BlockError> {
        let block = self.load(hash)?;
        let height = self.active.len() as u64;
        let undo = connect_block(&mut self.utxo, &block, height, self.params, &self.genesis_hash)?;
        self.active.push(*hash);
        self.undo.insert(*hash, undo);
        if height > self.undo_depth
            && let Some(old) = self.block_hash_at(height - self.undo_depth)
        {
            self.undo.remove(&old);
        }
        for tx in &block.transactions {
            self.tx_heights.insert(tx.txid(), height);
        }
        if let Some(entry) = self.index.get_mut(hash) {
            entry.status = BlockStatus::Valid;
        }
        Ok(block)
    }

    fn disconnect_tip(&mut self) -> Result<Block, BlockError> {
        let height = self.height();
        if height == 0 {
            return Err(BlockError::Storage("impossible d'annuler le bloc de genèse".into()));
        }
        let hash = self.tip_hash();
        let block = self.load(&hash)?;
        match self.undo.remove(&hash) {
            Some(undo) => {
                disconnect_block(&mut self.utxo, &block, &undo);
                self.active.pop();
                for tx in &block.transactions {
                    self.tx_heights.remove(&tx.txid());
                }
            }
            None => {
                // Données d'annulation élaguées : reconstruction complète de
                // l'état de la chaîne sans son sommet. En cas d'échec, rien
                // n'est modifié (la chaîne reste entière et cohérente).
                let shorter = &self.active[..self.active.len() - 1];
                let rebuilt = self.rebuild_state(shorter)?;
                self.active.pop();
                (self.utxo, self.undo, self.tx_heights) = rebuilt;
            }
        }
        Ok(block)
    }

    /// Reconstruit l'état (UTXO, données d'annulation, index des transactions)
    /// d'une chaîne en la rejouant depuis la genèse, **à part** : l'état
    /// courant n'est remplacé par l'appelant qu'en cas de réussite.
    #[allow(clippy::type_complexity)]
    fn rebuild_state(
        &self,
        chain: &[Hash256],
    ) -> Result<(UtxoSet, HashMap<Hash256, BlockUndo>, HashMap<Hash256, u64>), BlockError> {
        let mut utxo = UtxoSet::new();
        let mut undo_map = HashMap::new();
        let mut tx_heights = HashMap::new();
        for tx in &genesis_block(self.params).transactions {
            tx_heights.insert(tx.txid(), 0);
        }
        let tip_height = chain.len() as u64 - 1;
        for (h, hash) in chain.iter().enumerate().skip(1) {
            let height = h as u64;
            let block = self.load(hash)?;
            let undo = connect_block(&mut utxo, &block, height, self.params, &self.genesis_hash)
                .map_err(|e| BlockError::Storage(format!("reconstruction impossible au bloc {height} : {e}")))?;
            if tip_height - height < self.undo_depth {
                undo_map.insert(*hash, undo);
            }
            for tx in &block.transactions {
                tx_heights.insert(tx.txid(), height);
            }
        }
        Ok((utxo, undo_map, tx_heights))
    }

    /// Marque un bloc et tous ses descendants comme invalides.
    fn mark_invalid(&mut self, hash: &Hash256) {
        let mut stack = vec![*hash];
        while let Some(h) = stack.pop() {
            if let Some(entry) = self.index.get_mut(&h) {
                if entry.status == BlockStatus::Invalid {
                    continue;
                }
                entry.status = BlockStatus::Invalid;
            }
            if let Some(kids) = self.children.get(&h) {
                stack.extend(kids.iter().copied());
            }
        }
    }

    /// Dès que le bloc du plus haut point inscrit dans la version est connu, il
    /// devient le point de contrôle (sauf si un point signé plus haut et
    /// cohérent le couvre déjà).
    fn adopt_release_checkpoint(&mut self) {
        let Some(&(height, hash)) =
            self.params.release_checkpoints.iter().rev().find(|(_, h)| self.index.contains_key(h))
        else {
            return;
        };
        if self.index.get(&hash).is_none_or(|e| e.height != height || e.status == BlockStatus::Invalid) {
            return;
        }
        if let Some((cur_height, cur_hash)) = self.checkpoint {
            let consistent = if cur_height >= height {
                self.ancestor(&cur_hash, height) == Some(hash)
            } else {
                self.ancestor(&hash, cur_height) == Some(cur_hash)
            };
            if cur_height >= height || !consistent {
                return;
            }
        }
        self.checkpoint = Some((height, hash));
        self.recompute_best_candidate();
    }

    /// Meilleur prolongement (valide ou à essayer) du sommet actif.
    fn best_extension_of_tip(&self) -> Hash256 {
        let tip = self.tip_hash();
        let height = self.height();
        self.index
            .values()
            .filter(|e| {
                e.status != BlockStatus::Invalid
                    && e.height > height
                    && self.ancestor(&e.hash, height) == Some(tip)
                    && self.respects_checkpoint(&e.hash)
            })
            .max_by_key(|e| (e.chain_work, Reverse(e.seq)))
            .map_or(tip, |e| e.hash)
    }

    fn recompute_best_candidate(&mut self) {
        let tip = self.tip_hash();
        let key = |e: &IndexEntry| (e.chain_work, e.hash == tip, Reverse(e.seq));
        let best = self
            .index
            .values()
            .filter(|e| e.status != BlockStatus::Invalid && self.respects_checkpoint(&e.hash))
            .max_by_key(|e| key(e))
            .map_or(tip, |e| e.hash);
        self.best_candidate = best;
    }

    /// Localisateur de blocs (pour la synchronisation) depuis le sommet actif.
    pub fn locator(&self) -> Vec<Hash256> {
        self.locator_from(&self.tip_hash())
    }

    /// Localisateur depuis un bloc donné : les 10 derniers blocs, puis des
    /// pas qui doublent, et toujours la genèse à la fin.
    pub fn locator_from(&self, start: &Hash256) -> Vec<Hash256> {
        let mut out = Vec::new();
        let Some(mut entry) = self.index.get(start) else {
            return vec![self.genesis_hash];
        };
        let mut step = 1u64;
        loop {
            out.push(entry.hash);
            if entry.height == 0 || out.len() >= MAX_LOCATOR - 1 {
                break;
            }
            if out.len() >= 10 {
                step = step.saturating_mul(2);
            }
            let next_height = entry.height.saturating_sub(step);
            match self.ancestor(&entry.hash, next_height).and_then(|h| self.index.get(&h)) {
                Some(e) => entry = e,
                None => break,
            }
        }
        if out.last() != Some(&self.genesis_hash) {
            out.push(self.genesis_hash);
        }
        out
    }

    /// Blocs de la chaîne active qui suivent le premier bloc du localisateur
    /// présent dans notre chaîne active (au plus `max`, en s'arrêtant à `stop`).
    pub fn hashes_after_locator(&self, locator: &[Hash256], stop: &Hash256, max: usize) -> Vec<Hash256> {
        let start = locator.iter().find(|h| self.is_active(h)).and_then(|h| self.index.get(h)).map_or(0, |e| e.height);
        let mut out = Vec::new();
        for height in start + 1..=self.height() {
            let Some(hash) = self.block_hash_at(height) else { break };
            out.push(hash);
            if hash == *stop || out.len() >= max {
                break;
            }
        }
        out
    }

    /// Vérifie que l'ensemble UTXO correspond exactement à un rejeu complet de
    /// la chaîne active (diagnostic et tests).
    pub fn check_consistency(&self) -> Result<(), String> {
        let mut utxo = UtxoSet::new();
        for (h, hash) in self.active.iter().enumerate().skip(1) {
            let block = self.load(hash).map_err(|e| e.to_string())?;
            connect_block(&mut utxo, &block, h as u64, self.params, &self.genesis_hash)
                .map_err(|e| format!("bloc {h} : {e}"))?;
        }
        if utxo != self.utxo {
            return Err("l'ensemble UTXO ne correspond pas au rejeu de la chaîne".into());
        }
        for (h, hash) in self.active.iter().enumerate() {
            let entry = self.index.get(hash).ok_or("bloc actif absent de l'index")?;
            if entry.height != h as u64 || entry.status != BlockStatus::Valid {
                return Err(format!("entrée d'index incohérente à la hauteur {h}"));
            }
        }
        Ok(())
    }
}
