//! Mineur intégré au nœud (multi-fil).
//!
//! Le fil « mineur » prépare un modèle de bloc puis lance N fils de calcul,
//! chacun explorant une part des nonces. Le modèle est reconstruit dès que la
//! chaîne ou la mempool change, et au moins toutes les 10 secondes (pour
//! rafraîchir l'horodatage et inclure les nouvelles transactions).

use crate::node::Node;
use crate::{info, warn};
use kynovara_core::address::Address;
use kynovara_core::hash::Hash256;
use kynovara_core::miner::{MineResult, create_block_template, mine, mine_header};
use kynovara_core::time::now_secs;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Essais par lot avant de vérifier les signaux d'arrêt.
const BATCH: u64 = 1 << 16;
/// Durée maximale d'utilisation d'un même modèle de bloc.
const TEMPLATE_REFRESH: Duration = Duration::from_secs(10);
/// Délai minimal avant de reconstruire le modèle pour de nouvelles
/// transactions. Sans lui, un flot de transactions (légitime ou non) ferait
/// reconstruire le modèle sans arrêt et le minage n'avancerait plus. Un
/// nouveau sommet de chaîne, lui, est pris en compte immédiatement.
const MEMPOOL_REFRESH: Duration = Duration::from_secs(2);
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

/// Boucle du fil « mineur ».
pub(crate) fn miner_loop(node: &Node) {
    let miner = &node.inner.miner;
    while !node.is_shutting_down() {
        if !miner.running.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(200));
            continue;
        }
        let Some(address) = miner.address() else {
            warn!("mineur", "aucune adresse de récompense : minage arrêté");
            miner.running.store(false, Ordering::SeqCst);
            continue;
        };
        let threads = miner.threads();
        let epoch = miner.epoch.load(Ordering::SeqCst);
        let mempool_epoch = miner.mempool_epoch.load(Ordering::SeqCst);
        let template = {
            let st = node.state();
            create_block_template(&st.chain, &st.mempool, address.pubkey_hash(), now_secs(), COINBASE_TAG)
        };
        let header = template.block.header;
        let found = AtomicU64::new(u64::MAX);
        let stop = AtomicBool::new(false);
        let round_start = Instant::now();
        std::thread::scope(|scope| {
            for t in 0..threads {
                let (found, stop) = (&found, &stop);
                let spawned = std::thread::Builder::new().name(format!("minage-{t}")).spawn_scoped(scope, move || {
                    let step = threads as u64;
                    let mut start = t as u64;
                    loop {
                        match mine(&header, start, step, BATCH, stop, &miner.hashes) {
                            MineResult::Found(nonce) => {
                                found.store(nonce, Ordering::SeqCst);
                                stop.store(true, Ordering::SeqCst);
                                return;
                            }
                            MineResult::Stopped => return,
                            MineResult::Exhausted => start = start.wrapping_add(step.wrapping_mul(BATCH)),
                        }
                        let elapsed = round_start.elapsed();
                        let must_stop = stop.load(Ordering::SeqCst)
                            || node.is_shutting_down()
                            || !miner.running.load(Ordering::SeqCst)
                            || miner.epoch.load(Ordering::SeqCst) != epoch
                            || (elapsed > MEMPOOL_REFRESH
                                && miner.mempool_epoch.load(Ordering::SeqCst) != mempool_epoch)
                            || elapsed > TEMPLATE_REFRESH;
                        if must_stop {
                            stop.store(true, Ordering::SeqCst);
                            return;
                        }
                    }
                });
                if let Err(e) = spawned {
                    warn!("mineur", "impossible de démarrer un fil de minage : {e}");
                }
            }
        });
        let nonce = found.load(Ordering::SeqCst);
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
        for _ in 0..count {
            let mut block = {
                let st = self.state();
                create_block_template(&st.chain, &st.mempool, address.pubkey_hash(), now_secs(), b"regtest").block
            };
            mine_header(&mut block.header, 10_000_000).ok_or("nonce introuvable")?;
            let hash = block.hash();
            self.process_block(block, None).map_err(|e| format!("bloc refusé : {e}"))?;
            hashes.push(hash);
        }
        Ok(hashes)
    }
}
