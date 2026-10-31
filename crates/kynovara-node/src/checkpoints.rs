//! Points de contrôle signés côté nœud : vérification, application à la
//! chaîne, mémorisation sur le disque, alerte, relais, et signature
//! automatique de chaque nouveau sommet quand le nœud détient une clé de
//! signature déléguée (`signataire.json`). Les règles elles-mêmes sont dans
//! `kynovara_core::checkpoint` et `Chain::apply_checkpoint`.

use crate::node::Node;
use crate::p2p::message::{CHECKPOINT_PROTOCOL_VERSION, Message};
use crate::p2p::peer::{BAN_THRESHOLD, Peer};
use crate::{error, info, warn};
use kynovara_core::chain::CheckpointOutcome;
use kynovara_core::checkpoint::{Checkpoint, Delegation, Revocation};
use kynovara_core::crypto::{KeyPair, PublicKey};
use kynovara_core::encode::{Decode, DecodeError, Encode, Reader, put_len, put_u8};
use kynovara_core::hex;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use zeroize::Zeroizing;

const STATE_FILE: &str = "checkpoints.dat";
const SIGNER_FILE: &str = "signataire.json";
const STATE_VERSION: u8 = 1;
// Une révocation ne sert qu'en cas de vol de clé : quelques-unes suffisent.
// Toutes sont envoyées à chaque nouveau pair (limite de débit comprise).
const MAX_REVOCATIONS: usize = 256;

/// Clé de signature déléguée détenue par ce nœud.
pub(crate) struct Signer {
    pub key: KeyPair,
    pub delegation: Delegation,
}

/// État des points de contrôle de ce nœud.
#[derive(Default)]
pub(crate) struct CheckpointState {
    /// Dernier point appliqué à la chaîne.
    pub latest: Option<Checkpoint>,
    /// Dernière décision de la clé maîtresse : transmise à chaque pair, pour
    /// qu'un nœud absent pendant une alerte la reçoive avant les points qui
    /// l'ont suivie.
    pub last_master: Option<Checkpoint>,
    /// Point dont le bloc n'est pas encore connu (demandé aux pairs).
    pub pending: Option<Checkpoint>,
    pub revocations: Vec<Revocation>,
    pub revoked: HashSet<PublicKey>,
    /// Alerte : points contradictoires, ou point désignant un bloc invalide.
    /// La chaîne est alors gelée jusqu'à un point de la clé maîtresse.
    pub alert: Option<String>,
    pub signer: Option<Signer>,
}

/// Résultat du traitement d'un point de contrôle.
#[derive(Debug, PartialEq, Eq)]
pub enum CheckpointStatus {
    Applied,
    AlreadyKnown,
    Pending,
    /// Ignoré : alerte en cours (seule la clé maîtresse peut la lever).
    IgnoredDuringAlert,
    Conflict(String),
}

impl CheckpointState {
    fn encode(&self) -> Vec<u8> {
        let mut out = vec![STATE_VERSION];
        for slot in [&self.latest, &self.last_master] {
            match slot {
                Some(cp) => {
                    put_u8(&mut out, 1);
                    cp.encode_to(&mut out);
                }
                None => put_u8(&mut out, 0),
            }
        }
        put_len(&mut out, self.revocations.len());
        for r in &self.revocations {
            r.encode_to(&mut out);
        }
        match &self.alert {
            Some(text) => {
                put_u8(&mut out, 1);
                put_len(&mut out, text.len());
                out.extend_from_slice(text.as_bytes());
            }
            None => put_u8(&mut out, 0),
        }
        out
    }

    fn decode(bytes: &[u8]) -> Result<CheckpointState, DecodeError> {
        let mut r = Reader::new(bytes);
        if r.read_u8()? != STATE_VERSION {
            return Err(DecodeError::InvalidValue("version du fichier des points de contrôle"));
        }
        let mut st = CheckpointState::default();
        if r.read_u8()? == 1 {
            st.latest = Some(Checkpoint::decode_from(&mut r)?);
        }
        if r.read_u8()? == 1 {
            st.last_master = Some(Checkpoint::decode_from(&mut r)?);
        }
        let n = r.read_len(MAX_REVOCATIONS, 96)?;
        for _ in 0..n {
            let rev = Revocation::decode_from(&mut r)?;
            st.revoked.insert(rev.signer);
            st.revocations.push(rev);
        }
        if r.read_u8()? == 1 {
            let len = r.read_len(4096, 1)?;
            let text = r.read_bytes(len)?;
            st.alert = Some(String::from_utf8_lossy(text).into_owned());
        }
        r.finish()?;
        Ok(st)
    }
}

/// Écriture atomique d'un fichier réservé au propriétaire (0600 sous Unix).
fn write_private(path: &Path, data: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let io = |e: std::io::Error| format!("écriture de {} impossible : {e}", path.display());
    let mut file = options.open(&tmp).map_err(io)?;
    file.write_all(data).map_err(io)?;
    file.sync_all().map_err(io)?;
    drop(file);
    std::fs::rename(&tmp, path).map_err(io)
}

fn decode_hex_array<const N: usize>(value: Option<&Value>, what: &str) -> Result<Zeroizing<[u8; N]>, String> {
    let text = value.and_then(Value::as_str).ok_or_else(|| format!("champ « {what} » absent"))?;
    let bytes = Zeroizing::new(hex::decode(text).map_err(|e| format!("{what} : {e}"))?);
    let mut out = Zeroizing::new([0u8; N]);
    if bytes.len() != N {
        return Err(format!("{what} : {N} octets attendus"));
    }
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Lit `signataire.json` : `{"secret": hex(32), "delegation": hex(Delegation)}`.
fn parse_signer(text: &str) -> Result<Signer, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("JSON invalide : {e}"))?;
    let secret = decode_hex_array::<32>(v.get("secret"), "secret")?;
    let raw = v.get("delegation").and_then(Value::as_str).ok_or("champ « delegation » absent")?;
    let bytes = hex::decode(raw).map_err(|e| format!("delegation : {e}"))?;
    let delegation = Delegation::from_bytes(&bytes).map_err(|e| format!("delegation : {e}"))?;
    let key = KeyPair::from_secret_bytes(&secret);
    if key.public_key() != delegation.signer {
        return Err("la clé secrète ne correspond pas à la délégation".into());
    }
    Ok(Signer { key, delegation })
}

/// Le pair a fini la présentation et comprend les points de contrôle.
fn understands_checkpoints(peer: &Peer) -> bool {
    let info = peer.info();
    info.handshake_done() && info.version.as_ref().is_some_and(|v| v.version >= CHECKPOINT_PROTOCOL_VERSION)
}

impl Node {
    pub(crate) fn checkpoints(&self) -> std::sync::MutexGuard<'_, CheckpointState> {
        self.inner.checkpoints.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save_checkpoints(&self, st: &CheckpointState) {
        if let Err(e) = write_private(&self.inner.dir.join(STATE_FILE), &st.encode()) {
            error!("contrôle", "sauvegarde des points de contrôle impossible : {e}");
        }
    }

    /// Au démarrage : relit l'état enregistré et la clé de signature, puis
    /// réapplique le dernier point à la chaîne chargée depuis le disque.
    pub(crate) fn restore_checkpoints(&self) {
        let params = self.params();
        if params.checkpoint_master.is_none() {
            return;
        }
        let path = self.inner.dir.join(STATE_FILE);
        let saved = match std::fs::read(&path) {
            Ok(bytes) => match CheckpointState::decode(&bytes) {
                Ok(st) => st,
                Err(e) => {
                    warn!("contrôle", "{} illisible ({e}) : ignoré", path.display());
                    CheckpointState::default()
                }
            },
            Err(_) => CheckpointState::default(),
        };
        let signer_path = self.inner.dir.join(SIGNER_FILE);
        let signer = match std::fs::read_to_string(&signer_path) {
            Ok(text) => match parse_signer(&text) {
                Ok(s)
                    if s.delegation
                        .verify(&params.checkpoint_master.unwrap_or(PublicKey([0; 32])), &self.genesis()) =>
                {
                    info!(
                        "contrôle",
                        "clé de signature chargée ({}, valable jusqu'à la hauteur {})",
                        s.delegation.signer.to_hex(),
                        s.delegation.max_height
                    );
                    Some(s)
                }
                Ok(_) => {
                    warn!(
                        "contrôle",
                        "{} : délégation non signée par la clé maîtresse, ignorée",
                        signer_path.display()
                    );
                    None
                }
                Err(e) => {
                    warn!("contrôle", "{} illisible : {e}", signer_path.display());
                    None
                }
            },
            Err(_) => None,
        };
        let latest = saved.latest;
        let last_master = saved.last_master;
        let alert = saved.alert.clone();
        {
            let mut cps = self.checkpoints();
            *cps = saved;
            cps.latest = None;
            cps.alert = None;
            cps.signer = signer.filter(|s| !cps.revoked.contains(&s.delegation.signer));
        }
        // Le dernier point est réappliqué avant de rétablir une éventuelle
        // alerte (qui, sinon, le ferait ignorer).
        if let Some(cp) = last_master {
            let _ = self.process_checkpoint(cp, None);
        }
        if let Some(cp) = latest {
            let _ = self.process_checkpoint(cp, None);
        }
        if let Some(text) = alert {
            error!("contrôle", "ALERTE en cours (enregistrée) : {text}");
            self.state().chain.set_frozen(true);
            let mut cps = self.checkpoints();
            cps.alert = Some(text);
            self.save_checkpoints(&cps);
        }
        self.sign_tip();
    }

    fn genesis(&self) -> kynovara_core::hash::Hash256 {
        self.state().chain.genesis_hash()
    }

    /// Alerte en cours ?
    pub fn checkpoint_alert(&self) -> Option<String> {
        self.checkpoints().alert.clone()
    }

    /// Traite un point de contrôle (d'un pair, du RPC, ou signé ici).
    /// Soumet un point de contrôle local (RPC, tests).
    pub fn submit_checkpoint(&self, cp: Checkpoint) -> Result<CheckpointStatus, String> {
        self.process_checkpoint(cp, None)
    }

    /// Soumet une révocation locale (RPC, tests).
    pub fn submit_revocation(&self, rev: Revocation) -> Result<bool, String> {
        self.process_revocation(rev, None)
    }

    pub(crate) fn process_checkpoint(
        &self,
        cp: Checkpoint,
        source: Option<&Arc<Peer>>,
    ) -> Result<CheckpointStatus, String> {
        let params = self.params();
        let genesis = self.genesis();
        // Une décision de la clé maîtresse pendant une alerte est toujours examinée.
        let deciding = cp.is_master() && self.checkpoints().alert.is_some();
        if !deciding {
            let duplicate = {
                let cps = self.checkpoints();
                cps.latest == Some(cp) || cps.pending == Some(cp)
            };
            if duplicate {
                return Ok(CheckpointStatus::AlreadyKnown);
            }
            // Point plus ancien et cohérent avec l'historique signé (rediffusion) :
            // rien à faire, inutile de vérifier ses signatures.
            let covered = {
                let st = self.state();
                st.chain
                    .checkpoint()
                    .is_some_and(|(h, hash)| cp.height <= h && st.chain.ancestor(&hash, cp.height) == Some(cp.block))
            };
            if covered {
                return Ok(CheckpointStatus::AlreadyKnown);
            }
        }
        {
            let cps = self.checkpoints();
            if let Err(e) = cp.verify(params, &genesis, &cps.revoked) {
                drop(cps);
                if let Some(p) = source {
                    self.punish(p, BAN_THRESHOLD, &format!("point de contrôle refusé : {e}"));
                }
                return Err(e.to_string());
            }
            if cps.alert.is_some() && !cp.is_master() {
                return Ok(CheckpointStatus::IgnoredDuringAlert);
            }
        }

        let (outcome, height) = {
            let mut st = self.state();
            let was_frozen = st.chain.is_frozen();
            if cp.is_master() {
                st.chain.set_frozen(false);
            }
            let outcome = st.chain.apply_checkpoint(cp.height, cp.block, cp.is_master());
            let tip_change = match &outcome {
                CheckpointOutcome::Applied { tip_change, .. } | CheckpointOutcome::Conflict { tip_change, .. } => {
                    tip_change.as_ref()
                }
                _ => None,
            };
            if let Some(tc) = tip_change {
                crate::node::apply_tip_change(&mut st, tc);
                st.rejected.clear();
            }
            let resolved = cp.is_master()
                && matches!(outcome, CheckpointOutcome::Applied { .. } | CheckpointOutcome::AlreadyKnown);
            if matches!(outcome, CheckpointOutcome::Conflict { .. }) || (was_frozen && !resolved) {
                st.chain.set_frozen(true);
            }
            (outcome, st.chain.height())
        };

        let status = {
            let mut cps = self.checkpoints();
            let status = match &outcome {
                CheckpointOutcome::Applied { .. } => {
                    if cps.latest.is_none_or(|l| cp.height >= l.height || cp.is_master()) {
                        cps.latest = Some(cp);
                    }
                    if cps.pending.is_some_and(|p| p.height <= cp.height) {
                        cps.pending = None;
                    }
                    if cp.is_master() {
                        cps.last_master = Some(cp);
                        if cps.alert.take().is_some() {
                            info!("contrôle", "alerte levée par la clé maîtresse (hauteur {})", cp.height);
                        }
                    }
                    CheckpointStatus::Applied
                }
                CheckpointOutcome::AlreadyKnown => {
                    if cp.is_master() && cps.alert.take().is_some() {
                        cps.last_master = Some(cp);
                        info!("contrôle", "alerte levée par la clé maîtresse (hauteur {})", cp.height);
                    }
                    CheckpointStatus::AlreadyKnown
                }
                CheckpointOutcome::UnknownBlock => {
                    if cps.pending.is_none_or(|p| cp.height > p.height || cp.is_master()) {
                        cps.pending = Some(cp);
                    }
                    CheckpointStatus::Pending
                }
                CheckpointOutcome::Conflict { reason, .. } => {
                    let text = format!(
                        "point de contrôle contradictoire à la hauteur {} (bloc {}) : {reason}. Chaîne gelée, \
                         transactions suspendues : seule la clé maîtresse peut trancher.",
                        cp.height, cp.block
                    );
                    error!("contrôle", "ALERTE : {text}");
                    cps.alert = Some(text.clone());
                    CheckpointStatus::Conflict(text)
                }
            };
            self.save_checkpoints(&cps);
            status
        };

        match &status {
            CheckpointStatus::Applied => {
                info!("contrôle", "point de contrôle appliqué : hauteur {} ({})", cp.height, cp.block);
                if matches!(&outcome, CheckpointOutcome::Applied { tip_change: Some(_), .. }) {
                    self.inner.miner.bump();
                    info!("chaîne", "nouveau sommet imposé par le point de contrôle : hauteur {height}");
                }
                self.relay_checkpoint(&cp, source);
            }
            // Une contradiction signée est une preuve : tout le réseau doit la voir.
            CheckpointStatus::Conflict(_) => self.relay_checkpoint(&cp, source),
            CheckpointStatus::Pending => {
                if let Some(p) = source {
                    self.request_blocks(p, None, true);
                }
            }
            _ => {}
        }
        Ok(status)
    }

    /// Après l'arrivée de nouveaux blocs : réessaie le point en attente.
    pub(crate) fn retry_pending_checkpoint(&self) {
        let Some(cp) = self.checkpoints().pending else {
            return;
        };
        let known = self.state().chain.entry(&cp.block).is_some();
        if known {
            self.checkpoints().pending = None;
            let _ = self.process_checkpoint(cp, None);
        }
    }

    /// Traite une révocation de clé de signature.
    pub(crate) fn process_revocation(&self, rev: Revocation, source: Option<&Arc<Peer>>) -> Result<bool, String> {
        if self.checkpoints().revoked.contains(&rev.signer) {
            return Ok(false);
        }
        let genesis = self.genesis();
        if let Err(e) = rev.verify(self.params(), &genesis) {
            if let Some(p) = source {
                self.punish(p, BAN_THRESHOLD, &format!("révocation refusée : {e}"));
            }
            return Err(e.to_string());
        }
        {
            let mut cps = self.checkpoints();
            if !cps.revoked.insert(rev.signer) {
                return Ok(false);
            }
            if cps.revocations.len() >= MAX_REVOCATIONS {
                return Err("trop de révocations enregistrées".into());
            }
            cps.revocations.push(rev);
            if cps.signer.as_ref().is_some_and(|s| s.delegation.signer == rev.signer) {
                cps.signer = None;
                let _ = std::fs::remove_file(self.inner.dir.join(SIGNER_FILE));
                warn!("contrôle", "notre clé de signature vient d'être révoquée : plus aucune signature");
            }
            self.save_checkpoints(&cps);
        }
        info!("contrôle", "clé de signature révoquée : {}", rev.signer.to_hex());
        for peer in self.peer_list() {
            if source.is_some_and(|s| s.id == peer.id) || !understands_checkpoints(&peer) {
                continue;
            }
            peer.send(&Message::Revocation(Box::new(rev)));
        }
        Ok(true)
    }

    /// Installe une clé de signature déléguée (RPC) et signe le sommet.
    pub fn install_signer(&self, secret: &[u8; 32], delegation: Delegation) -> Result<PublicKey, String> {
        let params = self.params();
        let master = params.checkpoint_master.ok_or("points de contrôle désactivés sur ce réseau")?;
        if !delegation.verify(&master, &self.genesis()) {
            return Err("délégation non signée par la clé maîtresse de ce réseau".into());
        }
        let key = KeyPair::from_secret_bytes(secret);
        if key.public_key() != delegation.signer {
            return Err("la clé secrète ne correspond pas à la délégation".into());
        }
        if self.checkpoints().revoked.contains(&delegation.signer) {
            return Err("cette clé de signature a été révoquée".into());
        }
        let file = json!({
            "secret": hex::encode(secret),
            "delegation": hex::encode(&delegation.to_bytes()),
        });
        write_private(&self.inner.dir.join(SIGNER_FILE), file.to_string().as_bytes())?;
        let signer = delegation.signer;
        self.checkpoints().signer = Some(Signer { key, delegation });
        info!(
            "contrôle",
            "clé de signature installée ({}, jusqu'à la hauteur {})",
            signer.to_hex(),
            delegation.max_height
        );
        self.sign_tip();
        Ok(signer)
    }

    /// Signe le sommet actif si ce nœud détient une clé de signature valable.
    pub(crate) fn sign_tip(&self) {
        let params = self.params();
        let (height, tip, genesis) = {
            let st = self.state();
            (st.chain.height(), st.chain.tip_hash(), st.chain.genesis_hash())
        };
        let cp = {
            let cps = self.checkpoints();
            let Some(signer) = &cps.signer else {
                return;
            };
            if cps.alert.is_some()
                || height == 0
                || height > signer.delegation.max_height
                || height > params.checkpoint_sunset_height
                || cps.latest.is_some_and(|l| l.height >= height)
            {
                return;
            }
            let left = signer.delegation.max_height - height;
            if left < 1_440 && left % 100 == 0 {
                warn!(
                    "contrôle",
                    "la délégation de signature expire dans {left} blocs : relance « checkpoint delegate »"
                );
            }
            Checkpoint::sign_delegate(&signer.key, signer.delegation, &genesis, height, tip)
        };
        match self.process_checkpoint(cp, None) {
            Ok(_) => {}
            Err(e) => warn!("contrôle", "signature du sommet impossible : {e}"),
        }
    }

    fn relay_checkpoint(&self, cp: &Checkpoint, except: Option<&Arc<Peer>>) {
        for peer in self.peer_list() {
            if except.is_some_and(|e| e.id == peer.id) || !understands_checkpoints(&peer) {
                continue;
            }
            peer.send(&Message::Checkpoint(Box::new(*cp)));
        }
    }

    /// À la fin de la présentation : nos révocations et notre dernier point.
    pub(crate) fn send_checkpoints_to(&self, peer: &Peer) {
        if !understands_checkpoints(peer) {
            return;
        }
        let (revocations, last_master, latest) = {
            let cps = self.checkpoints();
            (cps.revocations.clone(), cps.last_master, cps.latest)
        };
        for r in revocations {
            peer.send(&Message::Revocation(Box::new(r)));
        }
        if let Some(cp) = last_master
            && Some(cp) != latest
        {
            peer.send(&Message::Checkpoint(Box::new(cp)));
        }
        if let Some(cp) = latest {
            peer.send(&Message::Checkpoint(Box::new(cp)));
        }
    }

    /// État des points de contrôle (RPC).
    pub fn checkpoint_status(&self) -> Value {
        let params = self.params();
        let cps = self.checkpoints();
        let show = |cp: &Checkpoint| {
            json!({
                "height": cp.height,
                "block": cp.block.to_hex(),
                "signer": match cp.delegate() { Some(k) => k.to_hex(), None => "maître".into() },
            })
        };
        json!({
            "enabled": params.checkpoint_master.is_some(),
            "master": params.checkpoint_master.map(|k| k.to_hex()),
            "sunset_height": params.checkpoint_sunset_height,
            "release": params.release_checkpoints.last().map(|(h, b)| json!({ "height": h, "block": b.to_hex() })),
            "latest": cps.latest.as_ref().map(show),
            "pending": cps.pending.as_ref().map(show),
            "alert": cps.alert,
            "revoked": cps.revocations.iter().map(|r| r.signer.to_hex()).collect::<Vec<_>>(),
            "signer": cps.signer.as_ref().map(|s| json!({
                "key": s.delegation.signer.to_hex(),
                "max_height": s.delegation.max_height,
            })),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kynovara_core::hash::Hash256;
    use kynovara_core::params::{REGTEST, REGTEST_CHECKPOINT_SECRET};

    #[test]
    fn state_file_roundtrip() {
        let master = KeyPair::from_secret_bytes(&REGTEST_CHECKPOINT_SECRET);
        let g = REGTEST.genesis.hash.unwrap();
        let mut st = CheckpointState {
            latest: Some(Checkpoint::sign_master(&master, &g, 3, Hash256([1; 32]))),
            alert: Some("contradiction à la hauteur 3".into()),
            ..Default::default()
        };
        let rev = Revocation::new(&master, &g, PublicKey([2; 32]));
        st.revoked.insert(rev.signer);
        st.revocations.push(rev);
        let back = CheckpointState::decode(&st.encode()).unwrap();
        assert_eq!(back.latest, st.latest);
        assert_eq!(back.revocations, st.revocations);
        assert!(back.revoked.contains(&PublicKey([2; 32])));
        assert_eq!(back.alert, st.alert);
        assert!(CheckpointState::decode(&[9]).is_err());
    }

    #[test]
    fn signer_file_must_match_its_delegation() {
        let master = KeyPair::from_secret_bytes(&REGTEST_CHECKPOINT_SECRET);
        let g = REGTEST.genesis.hash.unwrap();
        let hot = KeyPair::from_secret_bytes(&[4; 32]);
        let d = Delegation::new(&master, &g, hot.public_key(), 100);
        let good = json!({ "secret": hex::encode(&[4u8; 32]), "delegation": hex::encode(&d.to_bytes()) });
        assert!(parse_signer(&good.to_string()).is_ok());
        let other = json!({ "secret": hex::encode(&[5u8; 32]), "delegation": hex::encode(&d.to_bytes()) });
        assert!(parse_signer(&other.to_string()).is_err());
        assert!(parse_signer("{}").is_err());
    }
}
