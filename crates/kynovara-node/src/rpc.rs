//! Interface RPC locale (HTTP + JSON), utilisée par `kynovara-cli`.
//!
//! Sécurité :
//! - écoute sur 127.0.0.1 par défaut ;
//! - chaque requête doit porter `Authorization: Bearer <jeton>`, le jeton
//!   étant lu dans le fichier `.cookie` du dossier de données (recréé à chaque
//!   démarrage, lisible par l'utilisateur seul) ;
//! - comparaison du jeton en temps constant ;
//! - aucun en-tête CORS : une page web ne peut pas lire les réponses.
//!
//! Voir docs/RPC.md pour la liste des points d'accès.

use crate::http::{DeadlineReader, HttpError, Request, Response, read_request, write_response};
use crate::node::Node;
use crate::{debug, warn};
use kynovara_core::address::Address;
use kynovara_core::amount::format_amount;
use kynovara_core::block::Block;
use kynovara_core::checkpoint::{Checkpoint, Delegation, Revocation};
use kynovara_core::difficulty::difficulty;
use kynovara_core::encode::Decode;
use kynovara_core::hash::Hash256;
use kynovara_core::hex;
use kynovara_core::time::now_secs;
use kynovara_core::tx::{Transaction, TxKind};
use serde_json::{Value, json};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

const MAX_CONNECTIONS: usize = 32;
const MAX_ADDRESSES_PER_QUERY: usize = 2_000;
const MAX_GENERATE: u64 = 10_000;

/// Boucle d'acceptation des connexions RPC.
pub(crate) fn serve(node: &Node, listener: &TcpListener) {
    if let Err(e) = listener.set_nonblocking(true) {
        crate::error!("rpc", "configuration de l'écoute impossible : {e}");
        return;
    }
    let active = Arc::new(AtomicUsize::new(0));
    while !node.is_shutting_down() {
        match listener.accept() {
            Ok((stream, addr)) => {
                if active.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                    let mut stream = stream;
                    let _ = stream.set_nonblocking(false);
                    let _ = write_response(&mut stream, &error(503, "busy", "trop de requêtes simultanées"));
                    continue;
                }
                active.fetch_add(1, Ordering::SeqCst);
                let node = node.clone();
                let active_in_thread = Arc::clone(&active);
                let spawned = std::thread::Builder::new().name("rpc-requête".into()).spawn(move || {
                    handle_connection(&node, stream);
                    active_in_thread.fetch_sub(1, Ordering::SeqCst);
                });
                if spawned.is_err() {
                    active.fetch_sub(1, Ordering::SeqCst);
                    warn!("rpc", "impossible de traiter la requête de {addr}");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                debug!("rpc", "erreur d'acceptation : {e}");
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn handle_connection(node: &Node, mut stream: TcpStream) {
    let configured =
        stream.set_nonblocking(false).and_then(|()| stream.set_write_timeout(Some(Duration::from_secs(30))));
    if configured.is_err() {
        return;
    }
    // 10 secondes au total pour recevoir la requête entière.
    let request = read_request(&mut DeadlineReader::new(&stream, Instant::now() + Duration::from_secs(10)));
    let response = match request {
        Ok(request) => handle(node, &request),
        Err(HttpError::Status(code, message)) => error(code, "bad_request", message),
        Err(HttpError::Io(_)) => return,
    };
    let _ = write_response(&mut stream, &response);
}

fn ok(value: Value) -> Response {
    Response { status: 200, body: value.to_string() }
}

fn error(status: u16, code: &str, message: &str) -> Response {
    Response { status, body: json!({ "error": { "code": code, "message": message } }).to_string() }
}

/// Comparaison en temps constant (le temps de réponse ne révèle pas le jeton).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn json_body(request: &Request) -> Result<Value, Response> {
    serde_json::from_slice(&request.body).map_err(|e| error(400, "bad_json", &format!("JSON invalide : {e}")))
}

/// Aiguillage des requêtes.
pub(crate) fn handle(node: &Node, request: &Request) -> Response {
    let expected = format!("Bearer {}", node.cookie());
    let authorized =
        request.header("authorization").is_some_and(|v| constant_time_eq(v.as_bytes(), expected.as_bytes()));
    if !authorized {
        return error(401, "unauthorized", "jeton RPC absent ou invalide (voir le fichier .cookie du nœud)");
    }
    let path = request.path.trim_end_matches('/');
    let segments: Vec<&str> = path.split('/').skip(1).collect();
    let method = request.method.as_str();
    let result = match (method, segments.as_slice()) {
        ("GET", ["v1", "info"]) => Ok(ok(info(node))),
        ("GET", ["v1", "block", hash]) => block_by_hash(node, hash),
        ("GET", ["v1", "block-at", height]) => block_at(node, height),
        ("GET", ["v1", "tx", txid]) => tx_status(node, txid),
        ("POST", ["v1", "utxos"]) => json_body(request).and_then(|b| utxos(node, &b)),
        ("POST", ["v1", "tx"]) => json_body(request).and_then(|b| submit_tx(node, &b)),
        ("GET", ["v1", "mempool"]) => Ok(ok(mempool(node))),
        ("GET", ["v1", "peers"]) => Ok(ok(peers(node))),
        ("POST", ["v1", "peers"]) => json_body(request).and_then(|b| add_peer(node, &b)),
        ("GET", ["v1", "mining"]) => Ok(ok(mining_status(node))),
        ("POST", ["v1", "mining", "start"]) => json_body(request).and_then(|b| mining_start(node, &b)),
        ("POST", ["v1", "mining", "stop"]) => {
            node.stop_mining();
            Ok(ok(mining_status(node)))
        }
        ("POST", ["v1", "generate"]) => json_body(request).and_then(|b| generate(node, &b)),
        ("GET", ["v1", "checkpoint"]) => Ok(ok(node.checkpoint_status())),
        ("POST", ["v1", "checkpoint"]) => json_body(request).and_then(|b| submit_checkpoint(node, &b)),
        ("POST", ["v1", "revocation"]) => json_body(request).and_then(|b| submit_revocation(node, &b)),
        ("POST", ["v1", "signer"]) => json_body(request).and_then(|b| install_signer(node, &b)),
        ("POST", ["v1", "stop"]) => {
            node.shutdown();
            Ok(ok(json!({ "stopping": true })))
        }
        (
            _,
            [
                "v1",
                "info" | "mempool" | "peers" | "mining" | "utxos" | "tx" | "generate" | "stop" | "checkpoint"
                | "revocation" | "signer",
            ]
            | ["v1", "block" | "block-at" | "tx", _]
            | ["v1", "mining", "start" | "stop"],
        ) => Err(error(405, "method_not_allowed", "méthode HTTP non autorisée pour ce chemin")),
        _ => Err(error(404, "not_found", "chemin inconnu (voir docs/RPC.md)")),
    };
    result.unwrap_or_else(|e| e)
}

fn parse_hash(s: &str) -> Result<Hash256, Response> {
    Hash256::from_hex(s).map_err(|e| error(400, "bad_hash", &format!("empreinte invalide : {e}")))
}

fn parse_address(node: &Node, s: &str) -> Result<Address, Response> {
    Address::parse(s, node.params().network).map_err(|e| error(400, "bad_address", &e.to_string()))
}

fn info(node: &Node) -> Value {
    let params = node.params();
    let st = node.state();
    let tip = st.chain.tip();
    let peers = node.peer_list();
    let inbound = peers.iter().filter(|p| p.inbound && p.info().handshake_done()).count();
    let outbound = peers.iter().filter(|p| !p.inbound && p.info().handshake_done()).count();
    let miner = &node.inner.miner;
    json!({
        "network": params.network.name(),
        "version": env!("CARGO_PKG_VERSION"),
        "height": st.chain.height(),
        "tip": tip.hash.to_hex(),
        "tip_time": tip.header.timestamp,
        "target": tip.header.target.to_hex(),
        "difficulty": difficulty(params, &tip.header.target),
        "next_target": st.chain.next_target_after(&tip.hash).map(|t| t.to_hex()),
        "chain_work": tip.chain_work.to_hex(),
        "utxo_count": st.chain.utxo().len(),
        "supply": format_amount(u64::try_from(st.chain.utxo().total_value()).unwrap_or(u64::MAX)),
        "mempool": { "count": st.mempool.len(), "bytes": st.mempool.total_bytes() },
        "peers": { "inbound": inbound, "outbound": outbound },
        "mining": {
            "active": miner.running.load(Ordering::Relaxed),
            "threads": miner.threads(),
            "hashrate": miner.hashrate(),
            "blocks_found": miner.blocks_found.load(Ordering::Relaxed),
        },
        "transfers_unlock_height": params.transfers_unlock_height,
        "checkpoint": {
            "height": st.chain.checkpoint().map(|(h, _)| h),
            "frozen": st.chain.is_frozen(),
            "release_height": params.release_checkpoints.last().map(|(h, _)| h),
        },
        "time": now_secs(),
        "uptime": node.inner.started.elapsed().as_secs(),
    })
}

fn tx_json(node: &Node, tx: &Transaction) -> Value {
    let network = node.params().network;
    let inputs: Vec<Value> = tx
        .inputs()
        .iter()
        .map(|i| {
            json!({
                "txid": i.prev_out.txid.to_hex(),
                "vout": i.prev_out.vout,
                "address": Address::from_public_key(network, &i.public_key).encode(),
            })
        })
        .collect();
    let outputs: Vec<Value> = tx
        .outputs
        .iter()
        .map(|o| {
            json!({
                "value": o.value,
                "amount": format_amount(o.value),
                "address": Address::new(network, o.recipient).encode(),
            })
        })
        .collect();
    let mut v = json!({
        "txid": tx.txid().to_hex(),
        "size": tx.size(),
        "coinbase": tx.is_coinbase(),
        "inputs": inputs,
        "outputs": outputs,
    });
    if let TxKind::Coinbase { height, extra } = &tx.kind {
        v["coinbase_height"] = json!(height);
        v["coinbase_extra"] = json!(String::from_utf8_lossy(extra));
    }
    v
}

fn block_json(node: &Node, block: &Block, height: u64, active_height: u64, active: bool) -> Value {
    let h = &block.header;
    json!({
        "hash": block.hash().to_hex(),
        "height": height,
        "in_active_chain": active,
        "confirmations": if active { active_height + 1 - height } else { 0 },
        "header": {
            "version": h.version,
            "prev_hash": h.prev_hash.to_hex(),
            "merkle_root": h.merkle_root.to_hex(),
            "timestamp": h.timestamp,
            "target": h.target.to_hex(),
            "nonce": h.nonce,
        },
        "size": block.size(),
        "transactions": block.transactions.iter().map(|t| tx_json(node, t)).collect::<Vec<_>>(),
    })
}

fn block_by_hash(node: &Node, hash: &str) -> Result<Response, Response> {
    let hash = parse_hash(hash)?;
    let (block, height, active_height, active) = {
        let st = node.state();
        let Some(entry) = st.chain.entry(&hash) else {
            return Err(error(404, "not_found", "bloc inconnu"));
        };
        let height = entry.height;
        let block = st.chain.get_block(&hash).map_err(|e| error(500, "storage", &e))?;
        (block, height, st.chain.height(), st.chain.is_active(&hash))
    };
    let block = block.ok_or_else(|| error(404, "not_found", "bloc inconnu"))?;
    Ok(ok(block_json(node, &block, height, active_height, active)))
}

fn block_at(node: &Node, height: &str) -> Result<Response, Response> {
    let height: u64 = height.parse().map_err(|_| error(400, "bad_height", "hauteur invalide"))?;
    let hash = node
        .state()
        .chain
        .block_hash_at(height)
        .ok_or_else(|| error(404, "not_found", "hauteur supérieure à celle de la chaîne"))?;
    block_by_hash(node, &hash.to_hex())
}

fn tx_status(node: &Node, txid: &str) -> Result<Response, Response> {
    let txid = parse_hash(txid)?;
    let (in_mempool, location) = {
        let st = node.state();
        let mempool_tx = st.mempool.get(&txid).map(|e| (e.tx.clone(), e.fee));
        let location = st.chain.tx_height(&txid).and_then(|h| {
            let hash = st.chain.block_hash_at(h)?;
            let block = st.chain.get_block(&hash).ok()??;
            let tx = block.transactions.into_iter().find(|t| t.txid() == txid)?;
            Some((h, hash, tx, st.chain.height()))
        });
        (mempool_tx, location)
    };
    if let Some((tx, fee)) = in_mempool {
        let mut v = json!({ "status": "mempool", "fee": fee, "confirmations": 0 });
        v["tx"] = tx_json(node, &tx);
        return Ok(ok(v));
    }
    match location {
        Some((height, block, tx, tip)) => {
            let mut v = json!({
                "status": "confirmed",
                "height": height,
                "block": block.to_hex(),
                "confirmations": tip + 1 - height,
            });
            v["tx"] = tx_json(node, &tx);
            Ok(ok(v))
        }
        None => Err(error(404, "not_found", "transaction inconnue (ni en mempool, ni dans la chaîne active)")),
    }
}

fn utxos(node: &Node, body: &Value) -> Result<Response, Response> {
    let list = body
        .get("addresses")
        .and_then(Value::as_array)
        .ok_or_else(|| error(400, "bad_request", "champ « addresses » (liste) attendu"))?;
    if list.len() > MAX_ADDRESSES_PER_QUERY {
        return Err(error(400, "too_many", "trop d'adresses dans une seule requête"));
    }
    let mut addresses = Vec::with_capacity(list.len());
    for a in list {
        let s = a.as_str().ok_or_else(|| error(400, "bad_address", "adresse attendue (texte)"))?;
        addresses.push(parse_address(node, s)?);
    }
    let params = node.params();
    let st = node.state();
    let next_height = st.chain.height() + 1;
    let mut out = Vec::new();
    for addr in &addresses {
        for (op, entry) in st.chain.utxo().for_recipient(&addr.pubkey_hash()) {
            out.push(json!({
                "address": addr.encode(),
                "txid": op.txid.to_hex(),
                "vout": op.vout,
                "value": entry.output.value,
                "amount": format_amount(entry.output.value),
                "height": entry.height,
                "coinbase": entry.is_coinbase,
                "mature": entry.is_mature(next_height, params.coinbase_maturity),
                "spendable_at": if entry.is_coinbase { entry.height + params.coinbase_maturity } else { entry.height },
                "spent_in_mempool": st.mempool.spender_of(&op).map(|t| t.to_hex()),
            }));
        }
    }
    let mut incoming = Vec::new();
    let wanted: std::collections::HashSet<_> = addresses.iter().map(Address::pubkey_hash).collect();
    for entry in st.mempool.entries() {
        for (vout, o) in entry.tx.outputs.iter().enumerate() {
            if wanted.contains(&o.recipient) {
                incoming.push(json!({
                    "address": Address::new(params.network, o.recipient).encode(),
                    "txid": entry.txid.to_hex(),
                    "vout": vout,
                    "value": o.value,
                    "amount": format_amount(o.value),
                }));
            }
        }
    }
    Ok(ok(json!({
        "height": st.chain.height(),
        "coinbase_maturity": params.coinbase_maturity,
        "utxos": out,
        "mempool_incoming": incoming,
    })))
}

fn submit_tx(node: &Node, body: &Value) -> Result<Response, Response> {
    let raw =
        body.get("hex").and_then(Value::as_str).ok_or_else(|| error(400, "bad_request", "champ « hex » attendu"))?;
    let bytes = hex::decode(raw).map_err(|e| error(400, "bad_hex", &e.to_string()))?;
    let tx =
        Transaction::from_bytes(&bytes).map_err(|e| error(400, "bad_tx", &format!("transaction illisible : {e}")))?;
    match node.process_tx(tx, None) {
        Ok(txid) => Ok(ok(json!({ "txid": txid.to_hex() }))),
        Err(e) => Err(error(409, "rejected", &e.to_string())),
    }
}

fn hex_field(body: &Value, field: &str) -> Result<Vec<u8>, Response> {
    let raw = body
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| error(400, "bad_request", &format!("champ « {field} » attendu")))?;
    hex::decode(raw).map_err(|e| error(400, "bad_hex", &e.to_string()))
}

fn submit_checkpoint(node: &Node, body: &Value) -> Result<Response, Response> {
    let bytes = hex_field(body, "hex")?;
    let cp = Checkpoint::from_bytes(&bytes)
        .map_err(|e| error(400, "bad_checkpoint", &format!("point de contrôle illisible : {e}")))?;
    match node.submit_checkpoint(cp) {
        Ok(status) => Ok(ok(json!({ "status": format!("{status:?}"), "checkpoint": node.checkpoint_status() }))),
        Err(e) => Err(error(409, "rejected", &e)),
    }
}

fn submit_revocation(node: &Node, body: &Value) -> Result<Response, Response> {
    let bytes = hex_field(body, "hex")?;
    let rev = Revocation::from_bytes(&bytes)
        .map_err(|e| error(400, "bad_revocation", &format!("révocation illisible : {e}")))?;
    match node.submit_revocation(rev) {
        Ok(new) => Ok(ok(json!({ "revoked": rev.signer.to_hex(), "new": new }))),
        Err(e) => Err(error(409, "rejected", &e)),
    }
}

fn install_signer(node: &Node, body: &Value) -> Result<Response, Response> {
    let secret = Zeroizing::new(hex_field(body, "secret")?);
    let secret: &[u8; 32] =
        secret.as_slice().try_into().map_err(|_| error(400, "bad_secret", "clé secrète de 32 octets attendue"))?;
    let delegation = Delegation::from_bytes(&hex_field(body, "delegation")?)
        .map_err(|e| error(400, "bad_delegation", &format!("délégation illisible : {e}")))?;
    match node.install_signer(secret, delegation) {
        Ok(key) => Ok(ok(json!({ "signer": key.to_hex(), "max_height": delegation.max_height }))),
        Err(e) => Err(error(409, "rejected", &e)),
    }
}

fn mempool(node: &Node) -> Value {
    let st = node.state();
    let txids: Vec<String> = st.mempool.entries().map(|e| e.txid.to_hex()).collect();
    json!({ "count": st.mempool.len(), "bytes": st.mempool.total_bytes(), "txids": txids })
}

fn peers(node: &Node) -> Value {
    let list: Vec<Value> = node
        .peer_list()
        .iter()
        .map(|p| {
            let info = p.info();
            json!({
                "id": p.id,
                "address": p.addr.to_string(),
                "inbound": p.inbound,
                "ready": info.handshake_done(),
                "user_agent": info.version.as_ref().map(|v| v.user_agent.clone()),
                "height": info.best_height,
                "connected_secs": p.connected_at.elapsed().as_secs(),
                "misbehavior": info.misbehavior,
                "bytes_received": info.bytes_received,
            })
        })
        .collect();
    json!({ "peers": list })
}

fn add_peer(node: &Node, body: &Value) -> Result<Response, Response> {
    let s = body
        .get("address")
        .and_then(Value::as_str)
        .ok_or_else(|| error(400, "bad_request", "champ « address » (IP:PORT) attendu"))?;
    let addr = s.parse().map_err(|_| error(400, "bad_address", "format attendu : IP:PORT (ex. 192.168.1.20:19733)"))?;
    node.add_peer(addr);
    Ok(ok(json!({ "added": s })))
}

fn mining_status(node: &Node) -> Value {
    let miner = &node.inner.miner;
    json!({
        "active": miner.running.load(Ordering::Relaxed),
        "threads": miner.threads(),
        "address": miner.address().map(|a| a.encode()),
        "hashrate": miner.hashrate(),
        "blocks_found": miner.blocks_found.load(Ordering::Relaxed),
    })
}

fn mining_start(node: &Node, body: &Value) -> Result<Response, Response> {
    let address = match body.get("address").and_then(Value::as_str) {
        Some(s) => parse_address(node, s)?,
        None => node.inner.miner.address().ok_or_else(|| error(400, "bad_request", "champ « address » attendu"))?,
    };
    let threads = match body.get("threads") {
        None => node.inner.miner.threads(),
        Some(v) => v
            .as_u64()
            .filter(|n| (1..=1024).contains(n))
            .ok_or_else(|| error(400, "bad_threads", "« threads » doit être un entier entre 1 et 1024"))?
            as usize,
    };
    node.start_mining(address, threads).map_err(|e| error(400, "bad_request", &e))?;
    Ok(ok(mining_status(node)))
}

fn generate(node: &Node, body: &Value) -> Result<Response, Response> {
    let count = body
        .get("count")
        .and_then(Value::as_u64)
        .filter(|n| (1..=MAX_GENERATE).contains(n))
        .ok_or_else(|| error(400, "bad_count", "« count » doit être un entier entre 1 et 10000"))?;
    let address = body
        .get("address")
        .and_then(Value::as_str)
        .ok_or_else(|| error(400, "bad_request", "champ « address » attendu"))?;
    let address = parse_address(node, address)?;
    let hashes = node.generate(count as u32, &address).map_err(|e| error(400, "generate_failed", &e))?;
    Ok(ok(json!({ "blocks": hashes.iter().map(|h| h.to_hex()).collect::<Vec<_>>() })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_comparison() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }
}
