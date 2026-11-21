//! `kynovarad` : le nœud Kynovara.

#![forbid(unsafe_code)]

use kynovara_core::pow;
use kynovara_node::config::HELP;
use kynovara_node::{Command, Node, parse_args};
use std::process::{ExitCode, Stdio};

/// Variable d'environnement interne : le processus fait l'essai RandomX de
/// `check_randomx_jit` puis s'arrête.
const RANDOMX_SELF_TEST: &str = "KYNOVARA_RANDOMX_SELF_TEST";

fn main() -> ExitCode {
    if std::env::var_os(RANDOMX_SELF_TEST).is_some() {
        return if pow::self_test() { ExitCode::SUCCESS } else { ExitCode::FAILURE };
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config = match parse_args(&args) {
        Ok(Command::Run(config)) => config,
        Ok(Command::Help) => {
            print!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Ok(Command::Version) => {
            println!("kynovarad {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("Erreur : {e}");
            eprintln!("Aide : kynovarad --help");
            return ExitCode::from(2);
        }
    };
    eprintln!(
        "Kynovara {} — réseau {} — données : {}",
        env!("CARGO_PKG_VERSION"),
        config.network,
        config.network_dir().display()
    );
    if config.network.params().is_test_network() {
        eprintln!("{}", kynovara_core::params::TEST_NETWORK_NOTICE);
    }
    if config.network.params().randomx.is_some() {
        check_randomx_jit();
    }
    let node = match Node::start(*config) {
        Ok(node) => node,
        Err(e) => {
            eprintln!("Erreur au démarrage : {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "Nœud démarré. Arrêt conseillé : « kynovara-cli stop » (Ctrl+C fonctionne aussi, sans perte de données)."
    );
    node.wait();
    ExitCode::SUCCESS
}

/// Le compilateur à la volée de RandomX (JIT) a besoin de mémoire
/// exécutable. Certains systèmes durcis la refusent, et la bibliothèque ne
/// détecte pas ce refus : le processus planterait au premier bloc RandomX.
/// On essaie donc le JIT dans un processus fils ; s'il échoue, le nœud
/// utilise l'interpréteur (mêmes empreintes, plus lent).
fn check_randomx_jit() {
    if !pow::jit_allowed() {
        eprintln!("RandomX : compilateur à la volée désactivé ({}) ; interpréteur utilisé.", pow::JIT_ENV);
        return;
    }
    let child = std::env::current_exe().and_then(|exe| {
        std::process::Command::new(exe)
            .env(RANDOMX_SELF_TEST, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
    });
    match child {
        Ok(status) if status.success() => {}
        Ok(status) => {
            pow::set_jit_allowed(false);
            eprintln!(
                "RandomX : le système refuse le compilateur à la volée ({status}) ; interpréteur utilisé \
                 (mêmes résultats, plus lent)."
            );
        }
        Err(e) => eprintln!(
            "RandomX : essai du compilateur à la volée impossible ({e}). En cas de plantage, relance avec {}=0.",
            pow::JIT_ENV
        ),
    }
}
