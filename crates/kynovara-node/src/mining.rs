//! Mineur intégré au nœud (multi-fil).
//!
//! Le fil « mineur » prépare un modèle de bloc et le confie à N fils de
//! calcul, chacun explorant une part des nonces. Le modèle est reconstruit dès
//! que la chaîne ou la mempool change, et au moins toutes les 10 secondes
//! (pour rafraîchir l'horodatage et inclure les nouvelles transactions).
//!
//! Les fils de calcul sont permanents tant que le minage est actif : en
//! RandomX, chacun garde son calculateur (256 Mio, un tiers de seconde à
//! préparer) d'un modèle à l'autre et ne le recrée qu'au changement de clé.
//! Ils s'arrêtent, et libèrent leur mémoire, quand le minage s'arrête.

use crate::node::Node;
use crate::{info, warn};
use kynovara_core::address::Address;
use kynovara_core::block::BlockHeader;
use kynovara_core::hash::Hash256;
use kynovara_core::miner::{MineResult, create_block_template, mine, mine_header, mine_header_randomx, mine_randomx};
use kynovara_core::pow::RandomXHasher;
use kynovara_core::time::now_secs;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

/// Essais SHA-256 par lot avant de vérifier le signal d'arrêt.
const BATCH: u64 = 1 << 16;
/// Essais RandomX par lot (le signal d'arrêt est consulté à chaque essai).
const RANDOMX_BATCH: u64 = 1 << 20;
/// Durée maximale d'utilisation d'un même modèle de bloc.
const TEMPLATE_REFRESH: Duration = Duration::from_secs(10);
/// Délai minimal avant de reconstruire le modèle pour de nouvelles
/// transactions. Sans lui, un flot de transactions (légitime ou non) ferait
/// reconstruire le modèle sans arrêt et le minage n'avancerait plus. Un
/// nouveau sommet de chaîne, lui, est pris en compte immédiatement.
const MEMPOOL_REFRESH: Duration = Duration::from_secs(2);
/// Intervalle de surveillance d'un modèle par le fil « mineur ».
const POLL: Duration = Duration::from_millis(100);
/// Pause d'un fil de calcul dont le calculateur RandomX n'a pas pu être
/// préparé (mémoire insuffisante…), avant de réessayer au modèle suivant.
const RANDOMX_RETRY: Duration = Duration::from_secs(5);
/// Données libres placées dans la récompense des blocs minés.
const COINBASE_TAG: &[u8] = b"kynovara";

/// État partagé du mineur.
#[derive(Debug)]
pub(crate) struct MinerShared {
    pub running: AtomicBool,
    threads: AtomicUsize,
    address: Mutex<Option<Address>>,
    /// Incrémenté à chaque changement de sommet ou de configuration
    /// (reconstruction immédiate du modèle).
    epoch: AtomicU64,
    /// Incrémenté à chaque nouvelle transaction en mempool (reconstruction
    /// différée, voir `MEMPOOL_REFRESH`).
    mempool_epoch: AtomicU64,
    pub hashes: AtomicU64,
    pub blocks_found: AtomicU64,
    rate: Mutex<(Instant, u64, f64)>,
}

impl MinerShared {
    pub fn new(threads: usize) -> Self {
        MinerShared {
            running: AtomicBool::new(false),
            threads: AtomicUsize::new(threads.max(1)),
            address: Mutex::new(None),
            epoch: AtomicU64::new(0),
            mempool_epoch: AtomicU64::new(0),
            hashes: AtomicU64::new(0),
            blocks_found: AtomicU64::new(0),
            rate: Mutex::new((Instant::now(), 0, 0.0)),
        }
    }

    pub fn configure(&self, address: Address, threads: usize) {
        *self.address.lock().unwrap_or_else(|e| e.into_inner()) = Some(address);
        self.threads.store(threads.max(1), Ordering::SeqCst);
        self.bump();
    }

    pub fn address(&self) -> Option<Address> {
        *self.address.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn threads(&self) -> usize {
        self.threads.load(Ordering::SeqCst)
    }

    /// Signale qu'il faut reconstruire le modèle de bloc tout de suite
    /// (nouveau sommet, nouvelle configuration).
    pub fn bump(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }

    /// Signale de nouvelles transactions : le modèle sera reconstruit, au plus
    /// tôt `MEMPOOL_REFRESH` après le précédent.
    pub fn bump_mempool(&self) {
        self.mempool_epoch.fetch_add(1, Ordering::SeqCst);
    }

    /// Puissance de calcul mesurée (hachages par seconde).
    pub fn hashrate(&self) -> f64 {
        let mut rate = self.rate.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let total = self.hashes.load(Ordering::Relaxed);
        let elapsed = now.duration_since(rate.0).as_secs_f64();
        if elapsed >= 2.0 {
            rate.2 = (total - rate.1) as f64 / elapsed;
            *rate = (now, total, rate.2);
        }
        if self.running.load(Ordering::Relaxed) { rate.2 } else { 0.0 }
    }
}

/// Un modèle de bloc confié aux fils de calcul.
struct Round {
    header: BlockHeader,
    /// Clé RandomX (None : SHA-256 double).
    randomx_key: Option<Hash256>,
    /// Nombre de fils qui se partagent les nonces : le fil `i` essaie
    /// `i`, `i + n`, `i + 2n`…
    threads: u64,
    /// Nonce gagnant (`u64::MAX` : aucun).
    found: AtomicU64,
    /// Fin du modèle : nonce trouvé, ou modèle à reconstruire.
    stop: AtomicBool,
    /// Chaque fil de calcul y signale qu'il a fini ce modèle. Un canal par
    /// modèle : un signal tardif ne peut pas compter pour le modèle suivant.
    done: mpsc::Sender<()>,
}

impl Round {
    fn new(header: BlockHeader, randomx_key: Option<Hash256>, threads: u64, done: mpsc::Sender<()>) -> Self {
        Round { header, randomx_key, threads, found: AtomicU64::new(u64::MAX), stop: AtomicBool::new(false), done }
    }

    fn found(&self, nonce: u64) {
        self.found.store(nonce, Ordering::SeqCst);
        self.stop.store(true, Ordering::SeqCst);
    }

    /// Attend la fin du modèle, au plus `max`.
    fn wait_for_stop(&self, max: Duration) {
        let start = Instant::now();
        while !self.stop.load(Ordering::SeqCst) && start.elapsed() < max {
            std::thread::sleep(POLL);
        }
    }
}

/// Fil de calcul permanent et le canal qui lui confie les modèles (le fermer
/// arrête le fil).
struct Worker<'scope> {
    rounds: mpsc::Sender<Arc<Round>>,
    handle: std::thread::ScopedJoinHandle<'scope, ()>,
}

/// Signale au fil « mineur » la fin d'un modèle par un fil de calcul, même
/// si celui-ci s'interrompt brutalement.
struct RoundDone<'a>(&'a Round);

impl Drop for RoundDone<'_> {
    fn drop(&mut self) {
        let _ = self.0.done.send(());
    }
}

/// Boucle du fil « mineur ».
pub(crate) fn miner_loop(node: &Node) {
    let miner = &node.inner.miner;
    std::thread::scope(|scope| {
        let mut workers: Vec<Worker<'_>> = Vec::new();
        while !node.is_shutting_down() {
            if !miner.running.load(Ordering::SeqCst) {
                // Fermer les canaux arrête les fils, qui libèrent leur mémoire.
                workers.clear();
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            let Some(address) = miner.address() else {
                warn!("mineur", "aucune adresse de récompense : minage arrêté");
                miner.running.store(false, Ordering::SeqCst);
                continue;
            };
            // Un fil interrompu : on repart avec des fils neufs (le partage des
            // nonces suppose des numéros 0 à n − 1).
            if workers.iter().any(|w| w.handle.is_finished()) {
                workers.clear();
            }
            let wanted = miner.threads();
            workers.truncate(wanted);
            while workers.len() < wanted {
                let index = workers.len() as u64;
                let (rounds, jobs) = mpsc::channel::<Arc<Round>>();
                let hashes = &miner.hashes;
                let spawned = std::thread::Builder::new()
                    .name(format!("minage-{index}"))
                    .spawn_scoped(scope, move || worker_loop(index, &jobs, hashes));
                match spawned {
                    Ok(handle) => workers.push(Worker { rounds, handle }),
                    Err(e) => {
                        warn!("mineur", "impossible de démarrer un fil de minage : {e}");
                        break;
                    }
                }
            }
            if workers.is_empty() {
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }

            let epoch = miner.epoch.load(Ordering::SeqCst);
            let mempool_epoch = miner.mempool_epoch.load(Ordering::SeqCst);
            let template = {
                let st = node.state();
                create_block_template(&st.chain, &st.mempool, address.pubkey_hash(), now_secs(), COINBASE_TAG)
            };
            let (done_tx, done_rx) = mpsc::channel::<()>();
            let round =
                Arc::new(Round::new(template.block.header, template.randomx_key, workers.len() as u64, done_tx));
            let sent = workers.iter().filter(|w| w.rounds.send(Arc::clone(&round)).is_ok()).count();
            let round_start = Instant::now();
            let mut finished = 0;
            while finished < sent {
                if done_rx.recv_timeout(POLL).is_ok() {
                    finished += 1;
                }
                // Un fil mort (panique) avant d'avoir pris ce modèle ne le
                // signalera jamais : on ne l'attend pas (il est remplacé au
                // tour suivant).
                let dead = workers.iter().filter(|w| w.handle.is_finished()).count();
                if finished + dead >= sent {
                    round.stop.store(true, Ordering::SeqCst);
                    break;
                }
                let elapsed = round_start.elapsed();
                let must_stop = node.is_shutting_down()
                    || !miner.running.load(Ordering::SeqCst)
                    || miner.epoch.load(Ordering::SeqCst) != epoch
                    || (elapsed > MEMPOOL_REFRESH && miner.mempool_epoch.load(Ordering::SeqCst) != mempool_epoch)
                    || elapsed > TEMPLATE_REFRESH;
                if must_stop {
                    round.stop.store(true, Ordering::SeqCst);
                }
            }

            let nonce = round.found.load(Ordering::SeqCst);
            if nonce != u64::MAX {
                let mut block = template.block;
                block.header.nonce = nonce;
                let hash = block.hash();
                miner.blocks_found.fetch_add(1, Ordering::SeqCst);
                info!("mineur", "bloc trouvé à la hauteur {} : {hash}", template.height);
                if let Err(e) = node.process_block(block, None) {
                    crate::error!("mineur", "notre propre bloc a été refusé : {e}");
                }
            }
        }
        workers.clear();
    });
}

/// Boucle d'un fil de calcul : un modèle à la fois, jusqu'à la fermeture de
/// son canal.
fn worker_loop(index: u64, rounds: &mpsc::Receiver<Arc<Round>>, hashes: &AtomicU64) {
    // Calculateur RandomX de ce fil, gardé d'un modèle à l'autre.
    let mut hasher: Option<RandomXHasher> = None;
    while let Ok(round) = rounds.recv() {
        let _done = RoundDone(&round);
        let Some(key) = round.randomx_key else {
            mine_sha256d(index, &round, hashes);
            continue;
        };
        if hasher.as_ref().map(RandomXHasher::key) != Some(&key) {
            // Libérer l'ancien cache (256 Mio) avant de préparer le nouveau.
            hasher = None;
            match RandomXHasher::new(&key) {
                Ok(h) => hasher = Some(h),
                Err(e) => {
                    warn!("mineur", "fil {index} : {e}");
                    round.wait_for_stop(RANDOMX_RETRY);
                    continue;
                }
            }
        }
        if let Some(h) = &hasher {
            mine_with_randomx(index, &round, h, hashes);
        }
    }
}

fn mine_sha256d(index: u64, round: &Round, hashes: &AtomicU64) {
    let step = round.threads.max(1);
    let mut start = index;
    loop {
        match mine(&round.header, start, step, BATCH, &round.stop, hashes) {
            MineResult::Found(nonce) => return round.found(nonce),
            MineResult::Stopped => return,
            MineResult::Exhausted => start = start.wrapping_add(step.wrapping_mul(BATCH)),
        }
        if round.stop.load(Ordering::SeqCst) {
            return;
        }
    }
}

fn mine_with_randomx(index: u64, round: &Round, hasher: &RandomXHasher, hashes: &AtomicU64) {
    let step = round.threads.max(1);
    let mut start = index;
    loop {
        match mine_randomx(hasher, &round.header, start, step, RANDOMX_BATCH, &round.stop, hashes) {
            Ok(MineResult::Found(nonce)) => return round.found(nonce),
            Ok(MineResult::Stopped) => return,
            Ok(MineResult::Exhausted) => start = start.wrapping_add(step.wrapping_mul(RANDOMX_BATCH)),
            Err(e) => {
                warn!("mineur", "fil {index} : {e}");
                return round.wait_for_stop(RANDOMX_RETRY);
            }
        }
    }
}

impl Node {
    /// Démarre (ou reconfigure) le minage.
    pub fn start_mining(&self, address: Address, threads: usize) -> Result<(), String> {
        if address.network() != self.params().network {
            return Err("l'adresse de minage appartient à un autre réseau".into());
        }
        let miner = &self.inner.miner;
        miner.configure(address, threads);
        miner.running.store(true, Ordering::SeqCst);
        info!("mineur", "minage démarré : {} fil(s), récompenses vers {address}", miner.threads());
        if let Some(rx) = self.params().randomx {
            info!(
                "mineur",
                "RandomX (dès le bloc {}) : environ {} Mio de mémoire pour le minage",
                rx.activation_height,
                miner.threads() * 256
            );
        }
        Ok(())
    }

    pub fn stop_mining(&self) {
        if self.inner.miner.running.swap(false, Ordering::SeqCst) {
            info!("mineur", "minage arrêté");
        }
    }

    /// regtest seulement : mine immédiatement `count` blocs.
    pub fn generate(&self, count: u32, address: &Address) -> Result<Vec<Hash256>, String> {
        if self.params().network != kynovara_core::params::Network::Regtest {
            return Err("« generate » n'est disponible que sur regtest".into());
        }
        if address.network() != self.params().network {
            return Err("adresse d'un autre réseau".into());
        }
        let mut hashes = Vec::with_capacity(count as usize);
        let mut hasher: Option<RandomXHasher> = None;
        for _ in 0..count {
            let template = {
                let st = self.state();
                create_block_template(&st.chain, &st.mempool, address.pubkey_hash(), now_secs(), b"regtest")
            };
            let mut block = template.block;
            match template.randomx_key {
                None => {
                    mine_header(&mut block.header, 10_000_000).ok_or("nonce introuvable")?;
                }
                Some(key) => {
                    if hasher.as_ref().map(RandomXHasher::key) != Some(&key) {
                        // Libérer l'ancien cache avant de préparer le nouveau.
                        drop(hasher.take());
                        hasher = Some(RandomXHasher::new(&key).map_err(|e| e.to_string())?);
                    }
                    let h = hasher.as_ref().ok_or("calculateur RandomX indisponible")?;
                    mine_header_randomx(h, &mut block.header, 1_000_000)
                        .map_err(|e| e.to_string())?
                        .ok_or("nonce introuvable")?;
                }
            }
            let hash = block.hash();
            self.process_block(block, None).map_err(|e| format!("bloc refusé : {e}"))?;
            hashes.push(hash);
        }
        Ok(hashes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kynovara_core::pow::randomx_meets_target;
    use kynovara_core::u256::U256;

    fn header(target: U256) -> BlockHeader {
        BlockHeader {
            version: 1,
            prev_hash: Hash256([1; 32]),
            merkle_root: Hash256([2; 32]),
            timestamp: 1_800_000_000,
            target,
            nonce: 0,
        }
    }

    #[test]
    fn a_worker_mines_both_algorithms() {
        let (rounds_tx, rounds_rx) = mpsc::channel();
        let hashes = AtomicU64::new(0);
        let key = Hash256([3; 32]);
        std::thread::scope(|scope| {
            let counter = &hashes;
            scope.spawn(move || worker_loop(0, &rounds_rx, counter));
            let cases =
                [(None, U256::low_bits(248)), (Some(key), U256::low_bits(253)), (Some(key), U256::low_bits(253))];
            for (randomx_key, target) in cases {
                let (done_tx, done_rx) = mpsc::channel();
                let round = Arc::new(Round::new(header(target), randomx_key, 1, done_tx));
                rounds_tx.send(Arc::clone(&round)).unwrap();
                done_rx.recv_timeout(Duration::from_secs(120)).unwrap();
                let nonce = round.found.load(Ordering::SeqCst);
                assert_ne!(nonce, u64::MAX, "aucun nonce trouvé");
                let mut h = round.header;
                h.nonce = nonce;
                match randomx_key {
                    None => assert!(h.meets_target()),
                    Some(k) => assert!(randomx_meets_target(&h, &k).unwrap()),
                }
            }
            // Un modèle impossible s'arrête sur demande, sans nonce.
            let (done_tx, done_rx) = mpsc::channel();
            let round = Arc::new(Round::new(header(U256::ZERO), Some(key), 1, done_tx));
            rounds_tx.send(Arc::clone(&round)).unwrap();
            std::thread::sleep(Duration::from_millis(300));
            round.stop.store(true, Ordering::SeqCst);
            done_rx.recv_timeout(Duration::from_secs(30)).unwrap();
            assert_eq!(round.found.load(Ordering::SeqCst), u64::MAX);
            // Fermer le canal arrête le fil.
            drop(rounds_tx);
        });
        assert!(hashes.load(Ordering::SeqCst) > 0);
    }

    #[test]
    fn workers_stop_together_on_an_impossible_template() {
        // Deux fils sur un modèle impossible : tous deux s'arrêtent sur demande,
        // sans nonce. (Le partage des nonces lui-même est celui de `mine`, testé
        // dans kynovara-core.)
        let hashes = AtomicU64::new(0);
        let (done_tx, _done_rx) = mpsc::channel();
        let round = Round::new(header(U256::ZERO), None, 2, done_tx);
        std::thread::scope(|scope| {
            for index in 0..2 {
                let (round, hashes) = (&round, &hashes);
                scope.spawn(move || mine_sha256d(index, round, hashes));
            }
            std::thread::sleep(Duration::from_millis(100));
            round.stop.store(true, Ordering::SeqCst);
        });
        assert_eq!(round.found.load(Ordering::SeqCst), u64::MAX);
        assert!(hashes.load(Ordering::SeqCst) > 0);
    }

    #[test]
    fn a_late_completion_signal_cannot_count_for_the_next_template() {
        // Constat V-6 : chaque modèle a son propre canal de fin. Le signal d'un
        // fil qui termine l'ancien modèle part dans l'ancien canal.
        let (old_tx, old_rx) = mpsc::channel();
        let (new_tx, new_rx) = mpsc::channel::<()>();
        let old = Round::new(header(U256::ZERO), None, 1, old_tx);
        let _new = Round::new(header(U256::ZERO), None, 1, new_tx);
        drop(RoundDone(&old));
        assert!(old_rx.try_recv().is_ok());
        assert!(new_rx.try_recv().is_err());
    }
}
