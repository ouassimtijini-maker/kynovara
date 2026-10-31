//! Test de bout en bout : un vrai nœud regtest et le vrai binaire
//! `kynovara-cli` (création de portefeuilles, minage, envoi, restauration).
#![allow(clippy::unwrap_used)]

use kynovara_core::params::Network;
use kynovara_node::logger::Level;
use kynovara_node::{Node, NodeConfig};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PASSWORD: &str = "un-mot-de-passe-solide";

fn temp_dir() -> PathBuf {
    let mut b = [0u8; 8];
    getrandom::getrandom(&mut b).unwrap();
    let dir = std::env::temp_dir().join(format!("kynovara-e2e-{:x}", u64::from_le_bytes(b)));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

struct Cli<'a> {
    node: &'a Node,
    datadir: &'a Path,
}

impl Cli<'_> {
    /// Lance kynovara-cli ; `stdin` alimente les saisies (--password-stdin).
    fn run(&self, wallet: &str, args: &[&str], stdin: &str) -> (bool, String, String) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_kynovara-cli"));
        cmd.args(["--network", "regtest", "--datadir"])
            .arg(self.datadir)
            .args(["--rpc", &self.node.rpc_addr().to_string(), "--password-stdin", "--wallet"])
            .arg(self.datadir.join(wallet))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn ok(&self, wallet: &str, args: &[&str], stdin: &str) -> String {
        let (ok, out, err) = self.run(wallet, args, stdin);
        assert!(ok, "échec de {args:?}\nsortie : {out}\nerreurs : {err}");
        out
    }
}

fn pw() -> String {
    format!("{PASSWORD}\n")
}

fn line_value<'a>(out: &'a str, label: &str) -> &'a str {
    out.lines()
        .find(|l| l.starts_with(label))
        .and_then(|l| l.split(':').nth(1))
        .map(str::trim)
        .unwrap_or_else(|| panic!("« {label} » absent de :\n{out}"))
}

#[test]
fn full_wallet_scenario() {
    let datadir = temp_dir();
    let mut config = NodeConfig::default_for(Network::Regtest);
    config.datadir = datadir.clone();
    config.listen = None;
    config.rpc = "127.0.0.1:0".parse().unwrap();
    config.log_level = Level::Warn;
    let node = Node::start(config).unwrap();
    // Le CLI lit le jeton dans <datadir>/regtest/.cookie.
    let cli = Cli { node: &node, datadir: &datadir };

    // Création de deux portefeuilles.
    let out = cli.ok("alice.json", &["wallet", "create", "--no-verify"], &format!("{PASSWORD}\n{PASSWORD}\n"));
    let alice = line_value(&out, "Première adresse de réception").to_string();
    assert!(alice.starts_with("rkyn1"), "{alice}");
    let out = cli.ok("bob.json", &["wallet", "create", "--no-verify"], &format!("{PASSWORD}\n{PASSWORD}\n"));
    let bob = line_value(&out, "Première adresse de réception").to_string();
    // Pas d'écrasement d'un portefeuille existant.
    let (ok, _, err) =
        cli.run("alice.json", &["wallet", "create", "--no-verify"], &format!("{PASSWORD}\n{PASSWORD}\n"));
    assert!(!ok && err.contains("existe déjà"), "{err}");
    // Mot de passe trop court refusé.
    let (ok, _, _) = cli.run("carol.json", &["wallet", "create", "--no-verify"], "court\ncourt\n");
    assert!(!ok);

    // Minage de 101 blocs vers Alice : 2 récompenses deviennent dépensables.
    let out = cli.ok("alice.json", &["generate", "101", &alice], "");
    assert!(out.contains("101 bloc(s)"), "{out}");
    let out = cli.ok("alice.json", &["wallet", "balance"], &pw());
    assert_eq!(line_value(&out, "Disponible"), "100 KYNO");
    assert_eq!(line_value(&out, "En maturation"), "4950 KYNO");
    // Mauvais mot de passe.
    let (ok, _, err) = cli.run("alice.json", &["wallet", "balance"], "faux-mot-de-passe\n");
    assert!(!ok && err.contains("mot de passe incorrect"), "{err}");

    // Envoi de 12,5 KYNO à Bob (montant à la française).
    let out = cli.ok("alice.json", &["wallet", "send", &bob, "12,5", "--yes"], &pw());
    let txid = line_value(&out, "Transaction diffusée").to_string();
    assert_eq!(txid.len(), 64);
    let out = cli.ok("bob.json", &["wallet", "balance"], &pw());
    assert_eq!(line_value(&out, "À recevoir"), "12.5 KYNO");
    let out = cli.ok("alice.json", &["tx", &txid], "");
    assert!(out.contains("En attente"), "{out}");
    // Pièce engagée : un second envoi utilise une autre pièce.
    let out = cli.ok("alice.json", &["wallet", "send", &bob, "1", "--yes"], &pw());
    assert!(out.contains("Transaction diffusée"));

    // Confirmation.
    cli.ok("alice.json", &["generate", "1", &alice], "");
    let out = cli.ok("bob.json", &["wallet", "balance"], &pw());
    assert_eq!(line_value(&out, "Disponible"), "13.5 KYNO");
    let out = cli.ok("alice.json", &["tx", &txid], "");
    assert!(out.contains("Confirmée au bloc 102"), "{out}");
    // Fonds insuffisants.
    let (ok, _, err) = cli.run("bob.json", &["wallet", "send", &alice, "500", "--yes"], &pw());
    assert!(!ok && err.contains("fonds insuffisants"), "{err}");
    // Mauvais réseau pour le destinataire.
    let (ok, _, err) =
        cli.run("bob.json", &["wallet", "send", "tkyn1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqh7dktn", "1", "--yes"], &pw());
    assert!(!ok, "{err}");

    // Bob renvoie tout à Alice.
    cli.ok("bob.json", &["wallet", "send", &alice, "--all", "--yes"], &pw());
    cli.ok("alice.json", &["generate", "1", &alice], "");
    let out = cli.ok("bob.json", &["wallet", "balance"], &pw());
    assert_eq!(line_value(&out, "Disponible"), "0 KYNO");

    // Restauration du portefeuille d'Alice depuis sa phrase : même solde.
    let out = cli.ok("alice.json", &["wallet", "show-mnemonic"], &format!("{PASSWORD}\no\n"));
    let words: Vec<&str> = out.lines().filter_map(|l| l.split(". ").nth(1)).collect();
    assert_eq!(words.len(), 24);
    let before = cli.ok("alice.json", &["wallet", "balance"], &pw());
    cli.ok("alice-restauree.json", &["wallet", "restore"], &format!("{}\n{PASSWORD}\n{PASSWORD}\n", words.join(" ")));
    let after = cli.ok("alice-restauree.json", &["wallet", "balance"], &pw());
    assert_eq!(line_value(&before, "Disponible"), line_value(&after, "Disponible"));
    // Une phrase erronée est refusée.
    let mut bad = words.clone();
    bad.swap(0, 1);
    let (ok, _, _) = cli.run("x.json", &["wallet", "restore"], &format!("{}\n{PASSWORD}\n{PASSWORD}\n", bad.join(" ")));
    assert!(!ok);

    // Commandes d'information.
    let out = cli.ok("alice.json", &["info"], "");
    assert!(out.contains("Hauteur         : 103"), "{out}");
    let out = cli.ok("alice.json", &["block", "102"], "");
    assert!(out.contains(&txid), "{out}");
    let out = cli.ok("alice.json", &["wallet", "addresses"], &pw());
    assert!(out.contains("monnaie"), "{out}");
    cli.ok("alice.json", &["address", &alice], "");
    let (ok, _, _) = cli.run("alice.json", &["address", "rkyn1pasuneadresse"], "");
    assert!(!ok);

    // Limite d'écart : une adresse distribuée depuis une autre copie du
    // portefeuille (ici l'adresse n° 4) est découverte par l'original.
    std::fs::copy(datadir.join("alice.json"), datadir.join("alice-copie.json")).unwrap();
    let mut far = String::new();
    for _ in 0..4 {
        far = cli.ok("alice-copie.json", &["wallet", "address"], &pw()).trim().to_string();
    }
    let before = cli.ok("alice.json", &["wallet", "balance"], &pw());
    cli.ok("alice.json", &["generate", "1", &far], "");
    let after = cli.ok("alice.json", &["wallet", "balance"], &pw());
    // Disponible + en maturation : +50 KYNO exactement (la récompense du nouveau
    // bloc, payée à l'adresse lointaine ; une autre récompense a pu mûrir entre-temps).
    let total = |out: &str| {
        ["Disponible", "En maturation"]
            .iter()
            .map(|l| kynovara_core::amount::parse_amount(line_value(out, l).trim_end_matches(" KYNO")).unwrap())
            .sum::<u64>()
    };
    assert_eq!(total(&after), total(&before) + 50 * kynovara_core::amount::COIN, "{before}\n{after}");
    let out = cli.ok("alice.json", &["wallet", "addresses"], &pw());
    assert!(out.contains(&far), "l'adresse lointaine fait maintenant partie du portefeuille :\n{out}");

    // Minage en tâche de fond vers le portefeuille, puis arrêt.
    let out = cli.ok("alice.json", &["wallet", "mine", "--threads", "1"], &pw());
    assert!(out.contains("Minage ACTIF"), "{out}");
    let start = Instant::now();
    while node.tip().0 < 105 && start.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(node.tip().0 >= 105, "le mineur aurait dû trouver des blocs");
    let out = cli.ok("alice.json", &["mining", "stop"], "");
    assert!(out.contains("Minage arrêté"), "{out}");

    node.check_consistency().unwrap();
    cli.ok("alice.json", &["stop"], "");
    node.wait();
    let _ = std::fs::remove_dir_all(datadir);
}

#[test]
fn founder_checkpoint_commands() {
    let datadir = temp_dir();
    let mut config = NodeConfig::default_for(Network::Regtest);
    config.datadir = datadir.clone();
    config.listen = None;
    config.rpc = "127.0.0.1:0".parse().unwrap();
    config.log_level = Level::Error;
    let node = Node::start(config).unwrap();
    let cli = Cli { node: &node, datadir: &datadir };
    // Phrase publique de la clé maîtresse du regtest.
    let master = format!("{} art\n", ["abandon"; 23].join(" "));

    let out = cli.ok("w.json", &["checkpoint", "status"], "");
    assert!(out.contains("Dernier point     : aucun") && out.contains("ce nœud ne signe pas"), "{out}");
    let out = cli.ok("w.json", &["checkpoint", "master-pubkey"], &master);
    let expected = Network::Regtest.params().checkpoint_master.unwrap().to_hex();
    assert!(out.contains(&expected) && out.contains("C'est bien la clé maîtresse"), "{out}");

    // Une mauvaise phrase est refusée.
    let wrong = format!("{} abandon\n", ["zoo"; 23].join(" "));
    let (ok, _, err) = cli.run("w.json", &["checkpoint", "delegate"], &wrong);
    assert!(!ok, "{err}");

    // Délégation : le nœud signe ensuite chaque bloc.
    let out = cli.ok("w.json", &["checkpoint", "delegate", "--until", "1000"], &master);
    assert!(out.contains("Clé de signature installée"), "{out}");
    let addr = kynovara_core::address::Address::from_public_key(
        Network::Regtest,
        &kynovara_core::crypto::KeyPair::from_secret_bytes(&[1; 32]).public_key(),
    )
    .encode();
    cli.ok("w.json", &["generate", "3", &addr], "");
    let out = cli.ok("w.json", &["checkpoint", "status"], "");
    assert!(out.contains("Dernier point     : hauteur 3"), "{out}");
    let key = out.lines().find(|l| l.starts_with("Signature")).unwrap().split_whitespace().nth(3).unwrap().to_string();
    let info = cli.ok("w.json", &["info"], "");
    assert_eq!(line_value(&info, "Point contrôle"), "hauteur 3");
    let out = cli.ok("w.json", &["checkpoint", "export"], "");
    assert!(out.trim_start().starts_with("(3, Hash256::from_hex_const(\""), "{out}");
    let raw = cli.ok("w.json", &["rpc", "/v1/info"], "");
    let json: serde_json::Value = serde_json::from_str(raw.trim()).unwrap();
    assert_eq!(json["checkpoint"]["height"].as_u64(), Some(3));

    // Point de la clé maîtresse, puis révocation de la clé de signature.
    let out = cli.ok("w.json", &["checkpoint", "sign", "--yes"], &master);
    assert!(out.contains("Point de la clé maîtresse transmis"), "{out}");
    let out = cli.ok("w.json", &["checkpoint", "revoke", &key], &master);
    assert!(out.contains("révoquée"), "{out}");
    let out = cli.ok("w.json", &["checkpoint", "status"], "");
    assert!(out.contains("ce nœud ne signe pas") && out.contains(&format!("Clé révoquée      : {key}")), "{out}");

    node.shutdown();
    node.wait();
    let _ = std::fs::remove_dir_all(datadir);
}
