//! `kynovarad` : le nœud Kynovara.

#![forbid(unsafe_code)]

use kynovara_node::config::HELP;
use kynovara_node::{Command, Node, parse_args};
use std::process::ExitCode;

fn main() -> ExitCode {
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
