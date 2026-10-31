//! Configuration du nœud et lecture de la ligne de commande.

use crate::logger::Level;
use kynovara_core::address::Address;
use kynovara_core::params::Network;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

/// Configuration complète du nœud.
#[derive(Clone, Debug)]
pub struct NodeConfig {
    pub network: Network,
    /// Dossier racine des données (un sous-dossier par réseau).
    pub datadir: PathBuf,
    /// Adresse d'écoute P2P (None : aucune connexion entrante).
    pub listen: Option<SocketAddr>,
    /// Adresse d'écoute RPC.
    pub rpc: SocketAddr,
    /// Autoriser l'écoute RPC ailleurs qu'en boucle locale.
    pub rpc_allow_remote: bool,
    /// Pairs exclusifs (si non vide : aucune autre connexion sortante).
    pub connect: Vec<SocketAddr>,
    /// Pairs supplémentaires.
    pub addnode: Vec<SocketAddr>,
    /// Nœuds d'amorçage du réseau, joints comme des `addnode` quand `connect`
    /// est vide (vidé par `--noseed`).
    pub seeds: Vec<SocketAddr>,
    pub max_inbound: usize,
    pub max_outbound: usize,
    /// Démarrer le minage au lancement.
    pub mine: bool,
    pub mining_address: Option<Address>,
    pub mining_threads: usize,
    pub log_level: Level,
    /// Profondeur (en blocs) des branches concurrentes écrites sur le disque
    /// même si elles sont plus légères (voir `kynovara_core::chain`). Réglage
    /// avancé, sans option de ligne de commande.
    pub side_branch_margin: u64,
}

impl NodeConfig {
    /// Configuration par défaut d'un réseau.
    pub fn default_for(network: Network) -> NodeConfig {
        let p = network.params();
        let localhost = IpAddr::V4(Ipv4Addr::LOCALHOST);
        NodeConfig {
            network,
            datadir: default_datadir(),
            listen: Some(SocketAddr::new(localhost, p.default_p2p_port)),
            rpc: SocketAddr::new(localhost, p.default_rpc_port),
            rpc_allow_remote: false,
            connect: Vec::new(),
            addnode: Vec::new(),
            // Adresses vérifiées par les tests de `params` ; une adresse
            // illisible serait simplement ignorée.
            seeds: p.seeds.iter().filter_map(|s| s.parse().ok()).collect(),
            max_inbound: 32,
            max_outbound: 8,
            mine: false,
            mining_address: None,
            mining_threads: default_threads(),
            log_level: Level::Info,
            side_branch_margin: kynovara_core::chain::DEFAULT_SIDE_BRANCH_MARGIN,
        }
    }

    /// Dossier de données propre au réseau.
    pub fn network_dir(&self) -> PathBuf {
        self.datadir.join(self.network.name())
    }
}

/// Dossier de données par défaut : `%APPDATA%\Kynovara` sous Windows,
/// `~/.kynovara` ailleurs.
pub fn default_datadir() -> PathBuf {
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

/// Nombre de fils de minage par défaut : tous les cœurs sauf un, pour que
/// l'ordinateur reste utilisable.
pub fn default_threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get().saturating_sub(1).max(1))
}

/// Résultat de la lecture de la ligne de commande.
#[derive(Debug)]
pub enum Command {
    Run(Box<NodeConfig>),
    Help,
    Version,
}

pub const HELP: &str = "\
kynovarad — nœud de la blockchain Kynovara

Réseaux de test : les KYNO n'ont aucune valeur et ne sont pas à vendre (voir AVIS.md).

UTILISATION :
    kynovarad [OPTIONS]

OPTIONS :
    --network <RÉSEAU>        testnet (défaut), regtest ou mainnet
    --datadir <DOSSIER>       dossier des données
                              (défaut : %APPDATA%\\Kynovara ou ~/.kynovara)
    --listen <IP:PORT>        écoute pair-à-pair (défaut : 127.0.0.1 et le port du réseau)
    --no-listen               refuser toute connexion entrante
    --rpc <IP:PORT>           écoute RPC (défaut : 127.0.0.1 et le port RPC du réseau)
    --rpc-allow-remote        autoriser l'écoute RPC hors de 127.0.0.1 (déconseillé)
    --connect <IP:PORT>       se connecter UNIQUEMENT à ce pair (répétable)
    --addnode <IP:PORT>       se connecter aussi à ce pair (répétable)
    --noseed                  ne pas joindre les nœuds d'amorçage du réseau
                              (testnet : 51.222.143.109:19733)
    --max-inbound <N>         connexions entrantes maximales (défaut : 32)
    --max-outbound <N>        connexions sortantes maximales (défaut : 8)
    --mine                    miner dès le démarrage (exige --mining-address)
    --mining-address <ADR>    adresse qui reçoit les récompenses de minage
    --threads <N>             fils de minage (défaut : nombre de cœurs - 1)
    --log-level <NIVEAU>      error, warn, info (défaut) ou debug
    -h, --help                afficher cette aide
    -V, --version             afficher la version

PORTS PAR DÉFAUT :
    testnet : P2P 19733, RPC 19734
    regtest : P2P 29733, RPC 29734
";

/// Lit les arguments (sans le nom du programme).
pub fn parse_args(args: &[String]) -> Result<Command, String> {
    // Le réseau détermine les valeurs par défaut : on le lit d'abord.
    let mut network = Network::Testnet;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--network" {
            let name = args.get(i + 1).ok_or("--network attend une valeur")?;
            network = Network::from_name(name)
                .ok_or_else(|| format!("réseau inconnu : « {name} » (testnet, regtest ou mainnet)"))?;
        }
        i += 1;
    }
    let mut config = NodeConfig::default_for(network);
    let mut mining_address: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        let mut value = || -> Result<&String, String> {
            i += 1;
            args.get(i).ok_or_else(|| format!("{arg} attend une valeur"))
        };
        match arg {
            "-h" | "--help" => return Ok(Command::Help),
            "-V" | "--version" => return Ok(Command::Version),
            "--network" => {
                value()?;
            }
            "--datadir" => config.datadir = PathBuf::from(value()?),
            "--listen" => config.listen = Some(parse_socket(value()?)?),
            "--no-listen" => config.listen = None,
            "--rpc" => config.rpc = parse_socket(value()?)?,
            "--rpc-allow-remote" => config.rpc_allow_remote = true,
            "--connect" => config.connect.push(parse_socket(value()?)?),
            "--addnode" => config.addnode.push(parse_socket(value()?)?),
            "--noseed" => config.seeds.clear(),
            "--max-inbound" => config.max_inbound = parse_number(value()?, 0, 1000)?,
            "--max-outbound" => config.max_outbound = parse_number(value()?, 0, 64)?,
            "--mine" => config.mine = true,
            "--mining-address" => mining_address = Some(value()?.clone()),
            "--threads" => config.mining_threads = parse_number(value()?, 1, 1024)?,
            "--log-level" => {
                let v = value()?;
                config.log_level = Level::from_name(v).ok_or_else(|| format!("niveau de journal inconnu : « {v} »"))?;
            }
            other => return Err(format!("option inconnue : « {other} » (voir --help)")),
        }
        i += 1;
    }
    if let Some(a) = mining_address {
        config.mining_address = Some(Address::parse(&a, network).map_err(|e| e.to_string())?);
    }
    if config.mine && config.mining_address.is_none() {
        return Err("--mine exige --mining-address <adresse>".into());
    }
    if !config.rpc.ip().is_loopback() && !config.rpc_allow_remote {
        return Err(format!(
            "l'interface RPC doit écouter sur 127.0.0.1 ; pour {} ajoute --rpc-allow-remote (déconseillé)",
            config.rpc
        ));
    }
    Ok(Command::Run(Box::new(config)))
}

fn parse_socket(s: &str) -> Result<SocketAddr, String> {
    s.parse::<SocketAddr>()
        .map_err(|_| format!("adresse invalide : « {s} » (format attendu : IP:PORT, ex. 127.0.0.1:19733)"))
}

fn parse_number(s: &str, min: usize, max: usize) -> Result<usize, String> {
    let n: usize = s.parse().map_err(|_| format!("nombre invalide : « {s} »"))?;
    if n < min || n > max {
        return Err(format!("valeur hors limites : {n} (entre {min} et {max})"));
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn defaults_follow_network() {
        let Command::Run(c) = parse_args(&args("--network regtest")).unwrap() else { panic!() };
        assert_eq!(c.network, Network::Regtest);
        assert_eq!(c.rpc.port(), 29734);
        assert_eq!(c.listen.unwrap().port(), 29733);
        assert!(c.rpc.ip().is_loopback());
    }

    #[test]
    fn options() {
        let Command::Run(c) = parse_args(&args(
            "--network regtest --datadir /tmp/x --listen 0.0.0.0:1 --connect 127.0.0.1:2 --addnode 127.0.0.1:3 --threads 4 --log-level debug",
        ))
        .unwrap() else {
            panic!()
        };
        assert_eq!(c.datadir, PathBuf::from("/tmp/x"));
        assert_eq!(c.listen.unwrap().port(), 1);
        assert_eq!(c.connect.len(), 1);
        assert_eq!(c.addnode.len(), 1);
        assert_eq!(c.mining_threads, 4);
        assert_eq!(c.log_level, Level::Debug);
    }

    #[test]
    fn seeds_by_default_and_noseed() {
        let Command::Run(c) = parse_args(&args("")).unwrap() else { panic!() };
        assert_eq!(c.network, Network::Testnet);
        assert_eq!(c.seeds, vec!["51.222.143.109:19733".parse::<SocketAddr>().unwrap()]);
        let Command::Run(c) = parse_args(&args("--noseed")).unwrap() else { panic!() };
        assert!(c.seeds.is_empty());
        let Command::Run(c) = parse_args(&args("--network regtest")).unwrap() else { panic!() };
        assert!(c.seeds.is_empty(), "aucun nœud d'amorçage en regtest");
    }

    #[test]
    fn errors() {
        assert!(parse_args(&args("--network nope")).is_err());
        assert!(parse_args(&args("--listen pasuneadresse")).is_err());
        assert!(parse_args(&args("--mine")).is_err());
        assert!(parse_args(&args("--rpc 0.0.0.0:1")).is_err());
        assert!(parse_args(&args("--rpc 0.0.0.0:1 --rpc-allow-remote")).is_ok());
        assert!(parse_args(&args("--threads 0")).is_err());
        assert!(parse_args(&args("--inconnu")).is_err());
        assert!(parse_args(&args("--datadir")).is_err());
        // Adresse de minage d'un autre réseau.
        assert!(
            parse_args(&args("--network regtest --mining-address tkyn1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq"))
                .is_err()
        );
        assert!(matches!(parse_args(&args("--help")).unwrap(), Command::Help));
    }
}
