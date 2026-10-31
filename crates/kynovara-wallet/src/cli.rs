//! Commandes de `kynovara-cli`.

use crate::bip39;
use crate::keystore::{MIN_PASSWORD_LEN, UnlockedKey, WalletFile};
use crate::prompt::Prompter;
use crate::rpc_client::RpcClient;
use crate::wallet::{Branch, DEFAULT_FEE_RATE, Wallet, WalletUtxo, checkpoint_master_key};
use kynovara_core::address::Address;
use kynovara_core::amount::{TICKER, format_amount, parse_positive_amount};
use kynovara_core::checkpoint::{Checkpoint, DEFAULT_DELEGATION_BLOCKS, Delegation, Revocation};
use kynovara_core::crypto::{KeyPair, PublicKey};
use kynovara_core::encode::Encode;
use kynovara_core::genesis::genesis_header;
use kynovara_core::hash::Hash256;
use kynovara_core::hex;
use kynovara_core::miner::{MineResult, mine};
use kynovara_core::params::{ConsensusParams, GenesisParams, Network};
use kynovara_core::time::{format_utc, now_secs};
use kynovara_core::tx::OutPoint;
use serde_json::{Value, json};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use zeroize::Zeroizing;

pub const HELP: &str = "\
kynovara-cli — portefeuille et outil de commande de Kynovara

Réseaux de test : les KYNO n'ont aucune valeur et ne sont pas à vendre (voir AVIS.md).

UTILISATION :
    kynovara-cli [OPTIONS] <COMMANDE> [ARGUMENTS]

NŒUD :
    info                              état du nœud (hauteur, difficulté, pairs, minage)
    block <empreinte|hauteur>         afficher un bloc
    tx <txid>                         état d'une transaction
    peers                             pairs connectés
    addpeer <IP:PORT>                 se connecter à un pair
    mempool                           transactions en attente
    mining [status]                   état du minage
    mining start <adresse> [--threads N]
    mining stop
    generate <n> <adresse>            (regtest) miner n blocs immédiatement
    stop                              arrêter le nœud

PORTEFEUILLE :
    wallet create                     créer un portefeuille (phrase de 24 mots)
    wallet restore [--scan N]         restaurer à partir des 24 mots
    wallet address                    nouvelle adresse de réception
    wallet addresses                  lister les adresses
    wallet balance                    solde
    wallet send <adresse> <montant>   envoyer (montant en KYNO, ex. 12.5)
    wallet send <adresse> --all       envoyer tout le solde disponible
    wallet mine [--threads N]         miner vers une nouvelle adresse du portefeuille
    wallet show-mnemonic              réafficher la phrase de 24 mots (prudence !)

POINTS DE CONTRÔLE (fondateur) :
    checkpoint status                 état des points de contrôle du nœud
    checkpoint master-new             créer la clé maîtresse (nouvelle phrase de 24 mots, sur papier)
    checkpoint master-pubkey          réafficher la clé publique d'une clé maîtresse (24 mots demandés)
    checkpoint delegate [--until H]   autoriser ce nœud à signer chaque bloc (clé maîtresse demandée)
    checkpoint revoke <clé>           révoquer une clé de signature (clé maîtresse demandée)
    checkpoint export                 ligne à inscrire dans la prochaine version (release_checkpoints)
    checkpoint sign [<bloc>] [--height H]
                                      point signé par la clé maîtresse (lève une alerte)

OUTILS :
    address <adresse>                 vérifier une adresse
    rpc <chemin>                      réponse JSON brute d'une lecture RPC (ex. /v1/info), pour les scripts
    genesis [--timestamp T] [--message M] [--threads N]
                                      miner un bloc de genèse (lancement d'un réseau)

OPTIONS :
    --network <RÉSEAU>    testnet (défaut), regtest ou mainnet
    --datadir <DOSSIER>   dossier des données du nœud
    --rpc <IP:PORT>       adresse RPC du nœud (défaut : 127.0.0.1 et le port du réseau)
    --cookie <FICHIER>    fichier du jeton RPC (défaut : <datadir>/<réseau>/.cookie)
    --wallet <FICHIER>    fichier du portefeuille (défaut : <datadir>/<réseau>/wallet.json)
    --fee-rate <N>        frais en unités par octet (défaut : 2)
    --yes                 ne pas demander de confirmation avant un envoi
    --no-verify           ne pas faire vérifier la phrase de 24 mots à la création
    --password-stdin      lire mots de passe et réponses sur l'entrée standard (scripts)
    -h, --help            afficher cette aide
    -V, --version         afficher la version
";

/// Options globales.
#[derive(Debug)]
struct Options {
    network: Network,
    datadir: PathBuf,
    rpc: Option<SocketAddr>,
    cookie: Option<PathBuf>,
    wallet: Option<PathBuf>,
    fee_rate: u64,
    threads: Option<usize>,
    scan: u32,
    all: bool,
    yes: bool,
    no_verify: bool,
    timestamp: Option<u64>,
    message: Option<String>,
    until: Option<u64>,
    height: Option<u64>,
    prompter: Prompter,
    positionals: Vec<String>,
}

fn default_datadir() -> PathBuf {
    if cfg!(windows)
        && let Some(appdata) = std::env::var_os("APPDATA")
    {
        return PathBuf::from(appdata).join("Kynovara");
    }
    match std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        Some(home) => PathBuf::from(home).join(".kynovara"),
        None => PathBuf::from(".kynovara"),
    }
}

fn parse_options(args: &[String]) -> Result<Option<Options>, String> {
    let mut o = Options {
        network: Network::Testnet,
        datadir: default_datadir(),
        rpc: None,
        cookie: None,
        wallet: None,
        fee_rate: DEFAULT_FEE_RATE,
        threads: None,
        scan: 100,
        all: false,
        yes: false,
        no_verify: false,
        timestamp: None,
        message: None,
        until: None,
        height: None,
        prompter: Prompter { from_stdin: false },
        positionals: Vec::new(),
    };
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        let mut value = || -> Result<String, String> {
            i += 1;
            args.get(i).cloned().ok_or_else(|| format!("{arg} attend une valeur"))
        };
        match arg {
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("kynovara-cli {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "--network" => {
                let v = value()?;
                o.network = Network::from_name(&v).ok_or_else(|| format!("réseau inconnu : « {v} »"))?;
            }
            "--datadir" => o.datadir = PathBuf::from(value()?),
            "--rpc" => {
                let v = value()?;
                o.rpc = Some(v.parse().map_err(|_| format!("adresse RPC invalide : « {v} »"))?);
            }
            "--cookie" => o.cookie = Some(PathBuf::from(value()?)),
            "--wallet" => o.wallet = Some(PathBuf::from(value()?)),
            "--fee-rate" => {
                let v = value()?;
                o.fee_rate = v
                    .parse()
                    .ok()
                    .filter(|n| (1..=1_000_000).contains(n))
                    .ok_or_else(|| format!("frais par octet invalides : « {v} » (entier ≥ 1)"))?;
            }
            "--threads" => {
                let v = value()?;
                o.threads = Some(
                    v.parse()
                        .ok()
                        .filter(|n| (1..=1024).contains(n))
                        .ok_or_else(|| format!("nombre de fils invalide : « {v} »"))?,
                );
            }
            "--scan" => {
                let v = value()?;
                o.scan = v
                    .parse()
                    .ok()
                    .filter(|n| (1..=100_000).contains(n))
                    .ok_or_else(|| format!("valeur de --scan invalide : « {v} »"))?;
            }
            "--timestamp" => {
                let v = value()?;
                o.timestamp = Some(v.parse().map_err(|_| format!("horodatage invalide : « {v} »"))?);
            }
            "--message" => o.message = Some(value()?),
            "--until" => {
                let v = value()?;
                o.until = Some(v.parse().map_err(|_| format!("hauteur invalide : « {v} »"))?);
            }
            "--height" => {
                let v = value()?;
                o.height = Some(v.parse().map_err(|_| format!("hauteur invalide : « {v} »"))?);
            }
            "--all" => o.all = true,
            "--yes" | "-y" => o.yes = true,
            "--no-verify" => o.no_verify = true,
            "--password-stdin" => o.prompter.from_stdin = true,
            s if s.starts_with("--") => return Err(format!("option inconnue : « {s} » (voir --help)")),
            _ => o.positionals.push(arg.to_string()),
        }
        i += 1;
    }
    Ok(Some(o))
}

impl Options {
    fn net_dir(&self) -> PathBuf {
        self.datadir.join(self.network.name())
    }

    fn rpc(&self) -> Result<RpcClient, String> {
        let addr = self.rpc.unwrap_or_else(|| {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), self.network.params().default_rpc_port)
        });
        let cookie = self.cookie.clone().unwrap_or_else(|| self.net_dir().join(".cookie"));
        RpcClient::new(addr, &cookie)
    }

    fn wallet_path(&self) -> PathBuf {
        self.wallet.clone().unwrap_or_else(|| self.net_dir().join("wallet.json"))
    }
}

/// Point d'entrée.
pub fn run(args: &[String]) -> Result<(), String> {
    let Some(o) = parse_options(args)? else {
        return Ok(());
    };
    let command: Vec<&str> = o.positionals.iter().map(String::as_str).collect();
    match command.as_slice() {
        [] => {
            print!("{HELP}");
            Ok(())
        }
        ["info"] => cmd_info(&o),
        ["block", which] => cmd_block(&o, which),
        ["tx", txid] => cmd_tx(&o, txid),
        ["peers"] => cmd_peers(&o),
        ["addpeer", addr] => {
            let r = o.rpc()?.post("/v1/peers", &json!({ "address": addr }))?;
            println!("Pair ajouté : {}", r["added"].as_str().unwrap_or(addr));
            Ok(())
        }
        ["mempool"] => cmd_mempool(&o),
        ["mining"] | ["mining", "status"] => print_mining(&o.rpc()?.get("/v1/mining")?),
        ["mining", "start", address] => {
            let mut body = json!({ "address": address });
            if let Some(t) = o.threads {
                body["threads"] = json!(t);
            }
            print_mining(&o.rpc()?.post("/v1/mining/start", &body)?)
        }
        ["mining", "stop"] => print_mining(&o.rpc()?.post("/v1/mining/stop", &json!({}))?),
        ["generate", count, address] => {
            let count: u64 = count.parse().map_err(|_| format!("nombre invalide : « {count} »"))?;
            let r = o.rpc()?.post("/v1/generate", &json!({ "count": count, "address": address }))?;
            let blocks = r["blocks"].as_array().map_or(0, Vec::len);
            println!("{blocks} bloc(s) miné(s).");
            if let Some(last) = r["blocks"].as_array().and_then(|b| b.last()) {
                println!("Dernier bloc : {}", last.as_str().unwrap_or_default());
            }
            Ok(())
        }
        ["stop"] => {
            o.rpc()?.post("/v1/stop", &json!({}))?;
            println!("Arrêt du nœud demandé.");
            Ok(())
        }
        ["wallet", "create"] => cmd_wallet_create(&o),
        ["wallet", "restore"] => cmd_wallet_restore(&o),
        ["wallet", "address"] => cmd_wallet_address(&o),
        ["wallet", "addresses"] => cmd_wallet_addresses(&o),
        ["wallet", "balance"] => cmd_wallet_balance(&o),
        ["wallet", "send", to] if o.all => cmd_wallet_send(&o, to, None),
        ["wallet", "send", to, amount] => cmd_wallet_send(&o, to, Some(amount)),
        ["wallet", "mine"] => cmd_wallet_mine(&o),
        ["wallet", "show-mnemonic"] => cmd_show_mnemonic(&o),
        ["address", address] => cmd_address(address),
        ["genesis"] => cmd_genesis(&o),
        ["checkpoint"] | ["checkpoint", "status"] => cmd_checkpoint_status(&o),
        ["checkpoint", "master-new"] => cmd_checkpoint_master_new(&o),
        ["checkpoint", "master-pubkey"] => cmd_checkpoint_master_pubkey(&o),
        ["checkpoint", "delegate"] => cmd_checkpoint_delegate(&o),
        ["checkpoint", "revoke", key] => cmd_checkpoint_revoke(&o, key),
        ["checkpoint", "sign"] => cmd_checkpoint_sign(&o, None),
        ["checkpoint", "export"] => cmd_checkpoint_export(&o),
        ["rpc", path] if path.starts_with("/v1/") => {
            println!("{}", o.rpc()?.get(path)?);
            Ok(())
        }
        ["checkpoint", "sign", block] => cmd_checkpoint_sign(&o, Some(block)),
        _ => Err(format!("commande inconnue ou incomplète : « {} » (voir --help)", command.join(" "))),
    }
}

fn format_hashrate(h: f64) -> String {
    match h {
        h if h >= 1e9 => format!("{:.2} GH/s", h / 1e9),
        h if h >= 1e6 => format!("{:.2} MH/s", h / 1e6),
        h if h >= 1e3 => format!("{:.2} kH/s", h / 1e3),
        h => format!("{h:.0} H/s"),
    }
}

fn s(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "-".into(),
        other => other.to_string(),
    }
}

fn cmd_info(o: &Options) -> Result<(), String> {
    let r = o.rpc()?.get("/v1/info")?;
    println!("Réseau          : {}", s(&r["network"]));
    println!("Version         : {}", s(&r["version"]));
    println!("Hauteur         : {}", s(&r["height"]));
    println!("Dernier bloc    : {}", s(&r["tip"]));
    if let Some(t) = r["tip_time"].as_u64() {
        println!("Date du bloc    : {}", format_utc(t));
    }
    println!("Difficulté      : {:.3}", r["difficulty"].as_f64().unwrap_or(0.0));
    println!("Pièces émises   : {} {TICKER}", s(&r["supply"]));
    println!("Mempool         : {} transaction(s), {} octets", s(&r["mempool"]["count"]), s(&r["mempool"]["bytes"]));
    println!("Pairs           : {} entrant(s), {} sortant(s)", s(&r["peers"]["inbound"]), s(&r["peers"]["outbound"]));
    let mining = &r["mining"];
    if mining["active"].as_bool() == Some(true) {
        println!(
            "Minage          : ACTIF, {} fil(s), {}, {} bloc(s) trouvé(s)",
            s(&mining["threads"]),
            format_hashrate(mining["hashrate"].as_f64().unwrap_or(0.0)),
            s(&mining["blocks_found"])
        );
    } else {
        println!("Minage          : arrêté");
    }
    if o.network.params().is_test_network() {
        println!("Valeur          : aucune (réseau de test, rien n'est à vendre)");
    }
    if let Some(notice) = transfer_lock_notice(&r, o.network.params().target_spacing) {
        println!("Envois          : {notice}");
    }
    if let Some(h) = r["checkpoint"]["height"].as_u64() {
        let frozen = if r["checkpoint"]["frozen"].as_bool() == Some(true) { " — ALERTE, chaîne gelée" } else { "" };
        println!("Point contrôle  : hauteur {h}{frozen}");
    }
    if let (Some(release), Some(height)) = (r["checkpoint"]["release_height"].as_u64(), r["height"].as_u64())
        && height < release
    {
        println!("Synchronisation : incomplète (cette version connaît la chaîne jusqu'à la hauteur {release})");
    }
    Ok(())
}

fn print_tx(tx: &Value) {
    println!("  Transaction {}", s(&tx["txid"]));
    if tx["coinbase"].as_bool() == Some(true) {
        println!("    récompense de minage (hauteur {})", s(&tx["coinbase_height"]));
    }
    for i in tx["inputs"].as_array().into_iter().flatten() {
        println!("    entrée  : {}:{} ({})", s(&i["txid"]), s(&i["vout"]), s(&i["address"]));
    }
    for out in tx["outputs"].as_array().into_iter().flatten() {
        println!("    sortie  : {} {TICKER} -> {}", s(&out["amount"]), s(&out["address"]));
    }
}

fn cmd_block(o: &Options, which: &str) -> Result<(), String> {
    let client = o.rpc()?;
    let r = if which.len() == 64 {
        client.get(&format!("/v1/block/{which}"))?
    } else {
        let h: u64 = which.parse().map_err(|_| format!("ni empreinte ni hauteur : « {which} »"))?;
        client.get(&format!("/v1/block-at/{h}"))?
    };
    println!("Bloc {}", s(&r["hash"]));
    println!("  hauteur        : {}", s(&r["height"]));
    println!("  confirmations  : {}", s(&r["confirmations"]));
    println!("  parent         : {}", s(&r["header"]["prev_hash"]));
    if let Some(t) = r["header"]["timestamp"].as_u64() {
        println!("  date           : {}", format_utc(t));
    }
    println!("  cible          : {}", s(&r["header"]["target"]));
    println!("  nonce          : {}", s(&r["header"]["nonce"]));
    println!("  taille         : {} octets", s(&r["size"]));
    for tx in r["transactions"].as_array().into_iter().flatten() {
        print_tx(tx);
    }
    Ok(())
}

fn cmd_tx(o: &Options, txid: &str) -> Result<(), String> {
    let r = o.rpc()?.get(&format!("/v1/tx/{txid}"))?;
    match r["status"].as_str() {
        Some("mempool") => println!("En attente de confirmation (frais : {} unités)", s(&r["fee"])),
        _ => println!("Confirmée au bloc {} ({} confirmation(s))", s(&r["height"]), s(&r["confirmations"])),
    }
    print_tx(&r["tx"]);
    Ok(())
}

fn cmd_peers(o: &Options) -> Result<(), String> {
    let r = o.rpc()?.get("/v1/peers")?;
    let peers = r["peers"].as_array().cloned().unwrap_or_default();
    if peers.is_empty() {
        println!("Aucun pair connecté.");
    }
    for p in peers {
        println!(
            "{:22} {:8} hauteur {:>8}  {}  depuis {} s",
            s(&p["address"]),
            if p["inbound"].as_bool() == Some(true) { "entrant" } else { "sortant" },
            s(&p["height"]),
            s(&p["user_agent"]),
            s(&p["connected_secs"])
        );
    }
    Ok(())
}

fn cmd_mempool(o: &Options) -> Result<(), String> {
    let r = o.rpc()?.get("/v1/mempool")?;
    println!("{} transaction(s), {} octets", s(&r["count"]), s(&r["bytes"]));
    for t in r["txids"].as_array().into_iter().flatten() {
        println!("  {}", s(t));
    }
    Ok(())
}

fn print_mining(r: &Value) -> Result<(), String> {
    if r["active"].as_bool() == Some(true) {
        println!(
            "Minage ACTIF : {} fil(s), {}",
            s(&r["threads"]),
            format_hashrate(r["hashrate"].as_f64().unwrap_or(0.0))
        );
        println!("Récompenses vers : {}", s(&r["address"]));
    } else {
        println!("Minage arrêté.");
    }
    println!("Blocs trouvés depuis le démarrage du nœud : {}", s(&r["blocks_found"]));
    Ok(())
}

fn cmd_address(address: &str) -> Result<(), String> {
    match Address::parse_any(address) {
        Ok(a) => {
            println!("Adresse valide sur le réseau {}.", a.network());
            println!("Empreinte de clé : {}", a.pubkey_hash());
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}

// ----------------------------------------------------------------------------
// Portefeuille
// ----------------------------------------------------------------------------

fn new_password(o: &Options) -> Result<Zeroizing<String>, String> {
    loop {
        let p1 = o.prompter.password(&format!("Choisis un mot de passe ({MIN_PASSWORD_LEN} caractères minimum) : "))?;
        if p1.chars().count() < MIN_PASSWORD_LEN {
            eprintln!("Trop court.");
            if o.prompter.from_stdin {
                return Err("mot de passe trop court".into());
            }
            continue;
        }
        let p2 = o.prompter.password("Retape le mot de passe : ")?;
        if *p1 == *p2 {
            return Ok(p1);
        }
        eprintln!("Les deux mots de passe ne correspondent pas.");
        if o.prompter.from_stdin {
            return Err("les mots de passe ne correspondent pas".into());
        }
    }
}

struct Opened {
    file: WalletFile,
    unlocked: UnlockedKey,
    wallet: Wallet,
}

fn open_wallet(o: &Options) -> Result<Opened, String> {
    let path = o.wallet_path();
    if !path.exists() {
        return Err(format!(
            "aucun portefeuille à {} (crée-le avec « kynovara-cli --network {} wallet create »)",
            path.display(),
            o.network
        ));
    }
    let file = WalletFile::load(&path).map_err(|e| e.to_string())?;
    if file.network() != Some(o.network) {
        return Err(format!("ce portefeuille est pour le réseau {}, pas {}", file.network, o.network));
    }
    let password = o.prompter.password("Mot de passe du portefeuille : ")?;
    let unlocked = file.unlock(&password).map_err(|e| e.to_string())?;
    let wallet = Wallet::from_entropy(o.network, &unlocked.entropy, file.receive_index, file.change_index);
    Ok(Opened { file, unlocked, wallet })
}

fn save_indices(o: &Options, opened: &mut Opened) -> Result<(), String> {
    opened
        .file
        .update_indices(&opened.unlocked, opened.wallet.receive_index, opened.wallet.change_index)
        .and_then(|()| opened.file.save(&o.wallet_path()))
        .map_err(|e| e.to_string())
}

/// Adresses examinées au-delà des index connus (« limite d'écart ») : des
/// pièces reçues sur une adresse plus lointaine (portefeuille restauré,
/// adresse distribuée depuis une autre copie du portefeuille) font avancer les
/// index et deviennent visibles.
const LOOKAHEAD: u32 = 20;

/// Pièces du portefeuille, avec découverte des adresses utilisées au-delà des
/// index connus (les index avancés sont enregistrés).
fn wallet_utxos(o: &Options, opened: &mut Opened, client: &RpcClient) -> Result<(Vec<WalletUtxo>, Value), String> {
    for _ in 0..1_000 {
        let w = &opened.wallet;
        let receive_from = w.receive_index.max(1);
        let ahead: Vec<(Branch, u32, Address)> = (receive_from..receive_from.saturating_add(LOOKAHEAD))
            .map(|i| (Branch::Receive, i, w.address(Branch::Receive, i)))
            .chain(
                (w.change_index..w.change_index.saturating_add(LOOKAHEAD))
                    .map(|i| (Branch::Change, i, w.address(Branch::Change, i))),
            )
            .collect();
        let mut addresses: Vec<Address> = w.known_addresses().into_iter().map(|(_, _, a)| a).collect();
        addresses.extend(ahead.iter().map(|(_, _, a)| *a));
        let (utxos, incoming) = utxos_for(client, &addresses)?;
        let pending: Vec<&str> =
            incoming.as_array().map(|l| l.iter().filter_map(|i| i["address"].as_str()).collect()).unwrap_or_default();
        let mut advanced = false;
        for (branch, index, address) in &ahead {
            let used = utxos.iter().any(|u| u.recipient == address.pubkey_hash())
                || pending.iter().any(|p| *p == address.encode());
            if used {
                let slot = match branch {
                    Branch::Receive => &mut opened.wallet.receive_index,
                    Branch::Change => &mut opened.wallet.change_index,
                };
                if *slot <= *index {
                    *slot = index + 1;
                    advanced = true;
                }
            }
        }
        if !advanced {
            return Ok((utxos, incoming));
        }
        save_indices(o, opened)?;
    }
    Err("trop d'adresses utilisées au-delà des index enregistrés".into())
}

/// Deux positions distinctes tirées au hasard parmi `0..n` (n ≥ 2), triées.
/// Une erreur du générateur est remontée (et non ignorée : des octets nuls
/// donneraient toujours la même position, et la boucle ne finirait jamais).
fn two_random_positions(n: usize) -> Result<[usize; 2], String> {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).map_err(|e| format!("générateur aléatoire indisponible : {e}"))?;
    let r1 = u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
    let r2 = u64::from_le_bytes([b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]]);
    let first = (r1 % n as u64) as usize;
    // La seconde est choisie parmi les n - 1 autres positions : toujours distincte.
    let mut second = (r2 % (n as u64 - 1)) as usize;
    if second >= first {
        second += 1;
    }
    Ok(if first < second { [first, second] } else { [second, first] })
}

fn cmd_wallet_create(o: &Options) -> Result<(), String> {
    let path = o.wallet_path();
    if path.exists() {
        return Err(format!("un portefeuille existe déjà à {} : il ne sera pas écrasé", path.display()));
    }
    let password = new_password(o)?;
    let mut entropy = Zeroizing::new([0u8; 32]);
    getrandom::getrandom(&mut entropy[..]).map_err(|e| format!("générateur aléatoire indisponible : {e}"))?;
    let words = bip39::entropy_to_words(&entropy);
    show_phrase(&words, "PHRASE DE RÉCUPÉRATION (24 MOTS)");
    eprintln!("Écris ces 24 mots sur papier, dans l'ordre, et range-les en lieu sûr.");
    eprintln!("Quiconque les connaît peut prendre tes fonds. Sans eux, un fichier");
    eprintln!("de portefeuille perdu = fonds perdus. Ne les photographie pas et ne");
    eprintln!("les tape jamais sur un site web.");
    eprintln!();
    if !o.no_verify {
        confirm_phrase(o, &words, "le portefeuille n'a PAS été créé")?;
    }

    let mut file = WalletFile::create(o.network, &entropy, &password).map_err(|e| e.to_string())?;
    let mut wallet = Wallet::from_entropy(o.network, &entropy, 0, 0);
    let first = wallet.next_receive_address();
    let unlocked = file.unlock(&password).map_err(|e| e.to_string())?;
    file.update_indices(&unlocked, wallet.receive_index, wallet.change_index).map_err(|e| e.to_string())?;
    file.save(&path).map_err(|e| e.to_string())?;
    println!("Portefeuille créé : {}", path.display());
    println!("Première adresse de réception : {first}");
    Ok(())
}

/// Affiche une phrase de 24 mots, numérotée.
fn show_phrase(words: &[&str], title: &str) {
    eprintln!();
    eprintln!("================ {title} ================");
    for (i, chunk) in words.chunks(6).enumerate() {
        let line: Vec<String> =
            chunk.iter().enumerate().map(|(j, w)| format!("{:>2}. {w:<9}", i * 6 + j + 1)).collect();
        eprintln!("  {}", line.join(" "));
    }
    eprintln!("==================================================================");
}

/// Fait recopier deux mots tirés au hasard (trois essais chacun).
fn confirm_phrase(o: &Options, words: &[&str], failure: &str) -> Result<(), String> {
    for p in two_random_positions(words.len())? {
        let mut ok = false;
        for _ in 0..3 {
            let answer = o.prompter.line(&format!("Vérification — mot n° {} : ", p + 1))?;
            if answer.trim().eq_ignore_ascii_case(words[p]) {
                ok = true;
                break;
            }
            eprintln!("Ce n'est pas le bon mot.");
        }
        if !ok {
            return Err(format!("vérification échouée : {failure}, recommence"));
        }
    }
    Ok(())
}

fn utxos_for(client: &RpcClient, addresses: &[Address]) -> Result<(Vec<WalletUtxo>, Value), String> {
    let list: Vec<String> = addresses.iter().map(Address::encode).collect();
    let mut all = Vec::new();
    let mut last = Value::Null;
    for chunk in list.chunks(1_000) {
        let r = client.post("/v1/utxos", &json!({ "addresses": chunk }))?;
        for u in r["utxos"].as_array().into_iter().flatten() {
            let txid = Hash256::from_hex(u["txid"].as_str().unwrap_or_default()).map_err(|e| e.to_string())?;
            let address = Address::parse_any(u["address"].as_str().unwrap_or_default()).map_err(|e| e.to_string())?;
            all.push(WalletUtxo {
                outpoint: OutPoint { txid, vout: u["vout"].as_u64().unwrap_or(0) as u32 },
                value: u["value"].as_u64().unwrap_or(0),
                recipient: address.pubkey_hash(),
                mature: u["mature"].as_bool() == Some(true),
                spent_in_mempool: !u["spent_in_mempool"].is_null(),
            });
        }
        if let Some(mut incoming) = r["mempool_incoming"].as_array().cloned() {
            if let Value::Array(prev) = &mut last {
                prev.append(&mut incoming);
            } else {
                last = Value::Array(incoming);
            }
        }
    }
    Ok((all, last))
}

fn cmd_wallet_restore(o: &Options) -> Result<(), String> {
    let path = o.wallet_path();
    if path.exists() {
        return Err(format!("un portefeuille existe déjà à {} : il ne sera pas écrasé", path.display()));
    }
    eprintln!("Assure-toi que personne ne voit ton écran.");
    let phrase = o.prompter.line("Tape tes 24 mots, séparés par des espaces :\n> ")?;
    let entropy = bip39::words_to_entropy(&phrase).map_err(|e| e.to_string())?;
    let password = new_password(o)?;
    let mut wallet = Wallet::from_entropy(o.network, &entropy, 0, 0);

    // Recherche des adresses déjà utilisées (pièces encore présentes).
    match o.rpc() {
        Ok(client) => {
            let n = o.scan;
            let receive: Vec<Address> = (0..n).map(|i| wallet.address(Branch::Receive, i)).collect();
            let change: Vec<Address> = (0..n).map(|i| wallet.address(Branch::Change, i)).collect();
            let mut all = receive.clone();
            all.extend(change.iter().copied());
            let (utxos, _) = utxos_for(&client, &all)?;
            let last_used = |list: &[Address]| {
                list.iter()
                    .rposition(|a| utxos.iter().any(|u| u.recipient == a.pubkey_hash()))
                    .map_or(0, |i| i as u32 + 1)
            };
            wallet.receive_index = last_used(&receive);
            wallet.change_index = last_used(&change);
            let total = utxos.iter().map(|u| u.value).fold(0u64, u64::saturating_add);
            println!("{} pièce(s) retrouvée(s), {} {TICKER} au total.", utxos.len(), format_amount(total));
        }
        Err(e) => {
            eprintln!("Nœud injoignable ({e}) : les pièces seront retrouvées à la prochaine consultation du solde.");
        }
    }
    // Les adresses vidées avant la restauration sont indétectables (seules les
    // pièces encore présentes le sont) : toute la fenêtre examinée est donc
    // considérée comme utilisée, pour ne jamais redistribuer une adresse déjà
    // donnée ni perdre de vue un paiement futur sur l'une d'elles. Au-delà, la
    // limite d'écart découvre les adresses utilisées plus loin.
    wallet.receive_index = wallet.receive_index.max(o.scan);
    wallet.change_index = wallet.change_index.max(o.scan);
    let mut file = WalletFile::create(o.network, &entropy, &password).map_err(|e| e.to_string())?;
    let unlocked = file.unlock(&password).map_err(|e| e.to_string())?;
    file.update_indices(&unlocked, wallet.receive_index, wallet.change_index).map_err(|e| e.to_string())?;
    file.save(&path).map_err(|e| e.to_string())?;
    println!("Portefeuille restauré : {}", path.display());
    Ok(())
}

fn cmd_wallet_address(o: &Options) -> Result<(), String> {
    let mut opened = open_wallet(o)?;
    let address = opened.wallet.next_receive_address();
    save_indices(o, &mut opened)?;
    println!("{address}");
    Ok(())
}

fn cmd_wallet_addresses(o: &Options) -> Result<(), String> {
    let opened = open_wallet(o)?;
    for (branch, index, address) in opened.wallet.known_addresses() {
        let kind = match branch {
            Branch::Receive => "réception",
            Branch::Change => "monnaie",
        };
        println!("{kind:9} {index:>4}  {address}");
    }
    Ok(())
}

fn cmd_wallet_balance(o: &Options) -> Result<(), String> {
    let mut opened = open_wallet(o)?;
    let client = o.rpc()?;
    let (utxos, incoming) = wallet_utxos(o, &mut opened, &client)?;
    // Sommes saturées : les montants viennent du nœud, qui pourrait mentir.
    let sum = |f: &dyn Fn(&WalletUtxo) -> bool| {
        utxos.iter().filter(|u| f(u)).map(|u| u.value).fold(0u64, u64::saturating_add)
    };
    let available = sum(&|u| u.mature && !u.spent_in_mempool);
    let immature = sum(&|u| !u.mature);
    let outgoing = sum(&|u| u.spent_in_mempool);
    let incoming: u64 = incoming
        .as_array()
        .map(|list| list.iter().filter_map(|i| i["value"].as_u64()).fold(0u64, u64::saturating_add))
        .unwrap_or(0);
    println!("Disponible                 : {} {TICKER}", format_amount(available));
    println!("En maturation (minage)     : {} {TICKER}", format_amount(immature));
    println!("Engagé (envoi en attente)  : {} {TICKER}", format_amount(outgoing));
    println!("À recevoir (non confirmé)  : {} {TICKER}", format_amount(incoming));
    if immature > 0 {
        let p = o.network.params();
        println!("Les récompenses de minage deviennent dépensables {} blocs après leur bloc.", p.coinbase_maturity);
    }
    if let Some(notice) = transfer_lock_notice(&client.get("/v1/info")?, o.network.params().target_spacing) {
        println!("Envois                     : {notice}");
    }
    if o.network.params().is_test_network() {
        println!("Rappel : {TICKER} de test, sans valeur. Aucune vente, aucun investissement.");
    }
    Ok(())
}

/// Message de la période de rodage, si les envois sont encore bloqués au
/// prochain bloc.
fn transfer_lock_notice(info: &Value, spacing: u64) -> Option<String> {
    let unlock = info["transfers_unlock_height"].as_u64()?;
    let next = info["height"].as_u64()?.saturating_add(1);
    if next >= unlock {
        return None;
    }
    let minutes = (unlock - next).saturating_mul(spacing) / 60;
    let delay = match minutes {
        m if m >= 2 * 1440 => format!("environ {} jours", m / 1440),
        m if m >= 120 => format!("environ {} heures", m / 60),
        m => format!("environ {} minutes", m.max(1)),
    };
    Some(format!("bloqués jusqu'au bloc {unlock} (période de rodage, {delay})"))
}

fn cmd_wallet_send(o: &Options, to: &str, amount: Option<&str>) -> Result<(), String> {
    let to = Address::parse(to.trim(), o.network).map_err(|e| e.to_string())?;
    let amount = match amount {
        Some(a) => Some(parse_positive_amount(a.trim()).map_err(|e| e.to_string())?),
        None => None,
    };
    let client = o.rpc()?;
    if let Some(notice) = transfer_lock_notice(&client.get("/v1/info")?, o.network.params().target_spacing) {
        return Err(format!("envois {notice} : ton solde reste intact et le minage continue"));
    }
    let mut opened = open_wallet(o)?;
    let (utxos, _) = wallet_utxos(o, &mut opened, &client)?;
    let genesis = o.network.params().genesis.hash.ok_or("réseau non lancé")?;
    let prepared =
        opened.wallet.build_transaction(&utxos, &to, amount, o.fee_rate, &genesis).map_err(|e| e.to_string())?;

    println!("Destinataire : {to}");
    println!("Montant      : {} {TICKER}", format_amount(prepared.amount));
    println!("Frais        : {} {TICKER}", format_amount(prepared.fee));
    if let Some((addr, value)) = &prepared.change {
        println!("Monnaie      : {} {TICKER} -> {addr} (ton portefeuille)", format_amount(*value));
    }
    if !o.yes && !o.prompter.confirm("Confirmer l'envoi ?")? {
        println!("Envoi annulé.");
        return Ok(());
    }
    // L'index de monnaie est enregistré AVANT la diffusion.
    save_indices(o, &mut opened)?;
    let r = client.post("/v1/tx", &json!({ "hex": hex::encode(&prepared.tx.to_bytes()) }))?;
    println!("Transaction diffusée : {}", s(&r["txid"]));
    println!("Suivi : kynovara-cli --network {} tx {}", o.network, s(&r["txid"]));
    Ok(())
}

fn cmd_wallet_mine(o: &Options) -> Result<(), String> {
    let mut opened = open_wallet(o)?;
    let address = opened.wallet.next_receive_address();
    save_indices(o, &mut opened)?;
    let mut body = json!({ "address": address.encode() });
    if let Some(t) = o.threads {
        body["threads"] = json!(t);
    }
    print_mining(&o.rpc()?.post("/v1/mining/start", &body)?)
}

fn cmd_show_mnemonic(o: &Options) -> Result<(), String> {
    let opened = open_wallet(o)?;
    if !o.prompter.confirm("Afficher la phrase secrète à l'écran ? Personne ne doit la voir.")? {
        return Ok(());
    }
    let words = bip39::entropy_to_words(&opened.unlocked.entropy);
    for (i, w) in words.iter().enumerate() {
        println!("{:>2}. {w}", i + 1);
    }
    Ok(())
}

// ----------------------------------------------------------------------------
// Outil de genèse
// ----------------------------------------------------------------------------

fn cmd_genesis(o: &Options) -> Result<(), String> {
    let base = o.network.params();
    let message = o.message.clone().unwrap_or_else(|| base.genesis.message.to_string());
    if !message.is_ascii() || message.len() > kynovara_core::tx::MAX_COINBASE_EXTRA {
        return Err("le message doit être en ASCII et faire 64 caractères au maximum".into());
    }
    let timestamp = o.timestamp.unwrap_or_else(now_secs);
    let params: &'static ConsensusParams = Box::leak(Box::new(ConsensusParams {
        genesis: GenesisParams {
            timestamp,
            message: Box::leak(message.into_boxed_str()),
            nonce: 0,
            hash: None,
            target: base.genesis.target,
        },
        ..base.clone()
    }));
    let header = genesis_header(params, 0);
    let threads = o.threads.unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()));
    eprintln!("Minage de la genèse {} ({} fil(s), cible {})…", o.network, threads, params.genesis.target);
    let stop = AtomicBool::new(false);
    let hashes = AtomicU64::new(0);
    let found = AtomicU64::new(u64::MAX);
    let started = std::time::Instant::now();
    std::thread::scope(|scope| {
        for t in 0..threads {
            let (stop, hashes, found) = (&stop, &hashes, &found);
            scope.spawn(move || {
                if let MineResult::Found(n) = mine(&header, t as u64, threads as u64, u64::MAX, stop, hashes) {
                    found.store(n, Ordering::SeqCst);
                    stop.store(true, Ordering::SeqCst);
                }
            });
        }
    });
    let nonce = found.load(Ordering::SeqCst);
    let mut h = header;
    h.nonce = nonce;
    let secs = started.elapsed().as_secs_f64();
    println!("Genèse trouvée en {secs:.1} s ({} essais).", hashes.load(Ordering::SeqCst));
    println!("  horodatage : {timestamp} ({})", format_utc(timestamp));
    println!("  message    : {}", params.genesis.message);
    println!("  nonce      : {nonce}");
    println!("  empreinte  : {}", h.hash());
    println!();
    println!("À reporter dans crates/kynovara-core/src/params.rs :");
    println!("        timestamp: {timestamp},");
    println!("        nonce: {nonce},");
    println!("        message: {:?},", params.genesis.message);
    println!("        hash: Some(Hash256::from_hex_const(\"{}\")),", h.hash());
    Ok(())
}

// ----------------------------------------------------------------------------
// Points de contrôle (fondateur)
// ----------------------------------------------------------------------------

fn genesis_of(o: &Options) -> Result<Hash256, String> {
    o.network.params().genesis.hash.ok_or_else(|| "réseau non lancé".to_string())
}

/// Demande les 24 mots de la clé maîtresse et vérifie qu'ils correspondent à
/// celle inscrite dans les paramètres du réseau.
fn read_master(o: &Options) -> Result<KeyPair, String> {
    let expected = o.network.params().checkpoint_master.ok_or(
        "points de contrôle désactivés sur ce réseau : inscris d'abord la clé maîtresse \
         (checkpoint_master dans crates/kynovara-core/src/params.rs)",
    )?;
    eprintln!("Clé maîtresse : assure-toi que personne ne voit ton écran.");
    let phrase = o.prompter.line("Tape les 24 mots de la clé maîtresse, séparés par des espaces :\n> ")?;
    let entropy = bip39::words_to_entropy(&phrase).map_err(|e| e.to_string())?;
    let key = checkpoint_master_key(o.network, &entropy);
    if key.public_key() != expected {
        return Err("ces 24 mots ne correspondent pas à la clé maîtresse de ce réseau".into());
    }
    Ok(key)
}

fn cmd_checkpoint_master_new(o: &Options) -> Result<(), String> {
    let mut entropy = Zeroizing::new([0u8; 32]);
    getrandom::getrandom(&mut entropy[..]).map_err(|e| format!("générateur aléatoire indisponible : {e}"))?;
    let words = bip39::entropy_to_words(&entropy);
    show_phrase(&words, "CLÉ MAÎTRESSE — 24 MOTS");
    eprintln!("Ces 24 mots SONT la clé maîtresse : ils ne sont enregistrés nulle part.");
    eprintln!("Écris-les sur papier (deux copies, deux lieux sûrs), jamais sur un");
    eprintln!("ordinateur connecté. Qui les connaît contrôle les points de contrôle ;");
    eprintln!("sans eux, impossible de révoquer une clé volée ou de lever une alerte.");
    eprintln!();
    if !o.no_verify {
        confirm_phrase(o, &words, "aucune clé n'a été retenue")?;
    }
    let key = checkpoint_master_key(o.network, &entropy);
    println!("Clé maîtresse publique ({}) : {}", o.network, key.public_key().to_hex());
    println!("À inscrire dans checkpoint_master du réseau (crates/kynovara-core/src/params.rs).");
    Ok(())
}

fn cmd_checkpoint_master_pubkey(o: &Options) -> Result<(), String> {
    eprintln!("Clé maîtresse : assure-toi que personne ne voit ton écran.");
    let phrase = o.prompter.line("Tape les 24 mots de la clé maîtresse, séparés par des espaces :\n> ")?;
    let entropy = bip39::words_to_entropy(&phrase).map_err(|e| e.to_string())?;
    let key = checkpoint_master_key(o.network, &entropy).public_key();
    println!("Clé maîtresse publique ({}) : {}", o.network, key.to_hex());
    match o.network.params().checkpoint_master {
        Some(k) if k == key => println!("C'est bien la clé maîtresse inscrite pour ce réseau."),
        Some(_) => println!("Attention : ce n'est PAS la clé maîtresse inscrite pour ce réseau."),
        None => println!("Aucune clé maîtresse n'est encore inscrite pour ce réseau."),
    }
    Ok(())
}

fn cmd_checkpoint_delegate(o: &Options) -> Result<(), String> {
    let params = o.network.params();
    // La clé secrète de signature transite par le RPC (HTTP sans chiffrement) :
    // seulement vers la machine locale.
    if o.rpc.is_some_and(|a| !a.ip().is_loopback()) {
        return Err("par sécurité, « checkpoint delegate » ne parle qu'à un nœud local (127.0.0.1)".into());
    }
    let client = o.rpc()?;
    let height = client.get("/v1/info")?["height"].as_u64().unwrap_or(0);
    let max_height = o.until.unwrap_or(height.saturating_add(DEFAULT_DELEGATION_BLOCKS));
    if max_height <= height {
        return Err(format!("--until doit dépasser la hauteur actuelle ({height})"));
    }
    let master = read_master(o)?;
    let genesis = genesis_of(o)?;
    let mut secret = Zeroizing::new([0u8; 32]);
    getrandom::getrandom(&mut secret[..]).map_err(|e| format!("générateur aléatoire indisponible : {e}"))?;
    let hot = KeyPair::from_secret_bytes(&secret);
    let delegation = Delegation::new(&master, &genesis, hot.public_key(), max_height);
    let body = json!({
        "secret": hex::encode(&secret[..]),
        "delegation": hex::encode(&delegation.to_bytes()),
    });
    let r = client.post("/v1/signer", &body)?;
    let days = (max_height - height).saturating_mul(params.target_spacing) / 86_400;
    println!("Clé de signature installée sur le nœud : {}", s(&r["signer"]));
    println!("Valable jusqu'à la hauteur {max_height} (environ {days} jours) : le nœud signe chaque nouveau bloc.");
    println!("Avant l'échéance, relance « checkpoint delegate » pour la remplacer.");
    Ok(())
}

fn cmd_checkpoint_revoke(o: &Options, key: &str) -> Result<(), String> {
    let bytes = hex::decode(key).map_err(|e| format!("clé invalide : {e}"))?;
    let signer: [u8; 32] =
        bytes.try_into().map_err(|_| "clé invalide : 32 octets (64 caractères) attendus".to_string())?;
    let master = read_master(o)?;
    let rev = Revocation::new(&master, &genesis_of(o)?, PublicKey(signer));
    let r = o.rpc()?.post("/v1/revocation", &json!({ "hex": hex::encode(&rev.to_bytes()) }))?;
    println!("Clé de signature révoquée sur tout le réseau : {}", s(&r["revoked"]));
    Ok(())
}

fn cmd_checkpoint_sign(o: &Options, block: Option<&str>) -> Result<(), String> {
    let client = o.rpc()?;
    let b = match (block, o.height) {
        (Some(hash), _) => client.get(&format!("/v1/block/{hash}"))?,
        (None, Some(h)) => client.get(&format!("/v1/block-at/{h}"))?,
        (None, None) => {
            let h = client.get("/v1/info")?["height"].as_u64().ok_or("hauteur inconnue")?;
            client.get(&format!("/v1/block-at/{h}"))?
        }
    };
    let hash = Hash256::from_hex(&s(&b["hash"])).map_err(|e| e.to_string())?;
    let height = b["height"].as_u64().ok_or("hauteur du bloc inconnue")?;
    println!("Bloc désigné : hauteur {height}, {hash}");
    if !o.yes && !o.prompter.confirm("Signer ce point avec la clé maîtresse ? Il s'imposera à tout le réseau.")? {
        return Ok(());
    }
    let master = read_master(o)?;
    let cp = Checkpoint::sign_master(&master, &genesis_of(o)?, height, hash);
    let r = client.post("/v1/checkpoint", &json!({ "hex": hex::encode(&cp.to_bytes()) }))?;
    println!("Point de la clé maîtresse transmis : {}", s(&r["status"]));
    Ok(())
}

fn cmd_checkpoint_export(o: &Options) -> Result<(), String> {
    let r = o.rpc()?.get("/v1/checkpoint")?;
    let latest = &r["latest"];
    let height = latest["height"].as_u64().ok_or("aucun point de contrôle signé à exporter")?;
    let block = s(&latest["block"]);
    eprintln!("À ajouter à release_checkpoints du réseau {} (crates/kynovara-core/src/params.rs) :", o.network);
    println!("    ({height}, Hash256::from_hex_const(\"{block}\")),");
    Ok(())
}

fn cmd_checkpoint_status(o: &Options) -> Result<(), String> {
    let r = o.rpc()?.get("/v1/checkpoint")?;
    if r["enabled"].as_bool() != Some(true) {
        println!("Points de contrôle : désactivés sur ce réseau (aucune clé maîtresse inscrite).");
        return Ok(());
    }
    let show = |label: &str, v: &Value| {
        if !v.is_null() {
            println!("{label}: hauteur {}, bloc {}, signé par {}", s(&v["height"]), s(&v["block"]), s(&v["signer"]));
        }
    };
    println!("Clé maîtresse     : {}", s(&r["master"]));
    match &r["latest"] {
        Value::Null => println!("Dernier point     : aucun"),
        v => show("Dernier point     ", v),
    }
    show("En attente       ", &r["pending"]);
    if let Some(a) = r["alert"].as_str() {
        println!("ALERTE            : {a}");
    }
    match &r["signer"] {
        Value::Null => println!("Signature         : ce nœud ne signe pas"),
        v => println!("Signature         : clé {} (jusqu'à la hauteur {})", s(&v["key"]), s(&v["max_height"])),
    }
    for k in r["revoked"].as_array().into_iter().flatten() {
        println!("Clé révoquée      : {}", k.as_str().unwrap_or_default());
    }
    if !r["release"].is_null() {
        println!("Point de version  : hauteur {}, bloc {}", s(&r["release"]["height"]), s(&r["release"]["block"]));
    }
    println!("Fin des points    : hauteur {}", s(&r["sunset_height"]));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_lock_notice_counts_down() {
        let info = |height: u64| json!({ "height": height, "transfers_unlock_height": 6000 });
        assert_eq!(
            transfer_lock_notice(&info(1048), 120).unwrap(),
            "bloqués jusqu'au bloc 6000 (période de rodage, environ 6 jours)"
        );
        assert_eq!(
            transfer_lock_notice(&info(5900), 120).unwrap(),
            "bloqués jusqu'au bloc 6000 (période de rodage, environ 3 heures)"
        );
        assert!(transfer_lock_notice(&info(5998), 120).unwrap().contains("environ 2 minutes"));
        assert_eq!(transfer_lock_notice(&info(5999), 120), None, "le bloc 6000 peut contenir des envois");
        assert_eq!(transfer_lock_notice(&json!({ "height": 3 }), 120), None);
    }

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn option_parsing() {
        let o = parse_options(&args("--network regtest --fee-rate 5 wallet send rkyn1x 1.5 --yes")).unwrap().unwrap();
        assert_eq!(o.network, Network::Regtest);
        assert_eq!(o.fee_rate, 5);
        assert!(o.yes);
        assert_eq!(o.positionals, ["wallet", "send", "rkyn1x", "1.5"]);
        assert!(parse_options(&args("--fee-rate 0")).is_err());
        assert!(parse_options(&args("--network lune")).is_err());
        assert!(parse_options(&args("--inconnu")).is_err());
        assert!(parse_options(&args("--rpc")).is_err());
        assert!(parse_options(&args("--help")).unwrap().is_none());
    }

    #[test]
    fn unknown_command() {
        assert!(run(&args("--network regtest bidule")).is_err());
        assert!(run(&args("wallet send")).is_err());
    }

    #[test]
    fn verification_positions_are_distinct_and_in_range() {
        for _ in 0..2_000 {
            let [a, b] = two_random_positions(24).unwrap();
            assert!(a < b && b < 24, "{a} {b}");
        }
        assert_eq!(two_random_positions(2).unwrap(), [0, 1]);
    }

    #[test]
    fn hashrate_format() {
        assert_eq!(format_hashrate(12.0), "12 H/s");
        assert_eq!(format_hashrate(12_500.0), "12.50 kH/s");
        assert_eq!(format_hashrate(3.2e9), "3.20 GH/s");
    }
}
