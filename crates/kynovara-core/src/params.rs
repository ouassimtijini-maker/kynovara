//! Paramètres de consensus de chaque réseau.
//!
//! Trois réseaux :
//! - **mainnet** : le futur réseau principal. Il n'est **pas encore lancé** :
//!   son bloc de genèse sera créé lors de la cérémonie de lancement (voir
//!   docs/LANCEMENT.md). Le nœud refuse de démarrer dessus d'ici là ;
//! - **testnet** : réseau de test avec une vraie difficulté, pour miner sur
//!   ton PC ou entre quelques machines ;
//! - **regtest** : réseau de développement local où les blocs se minent
//!   instantanément (tests automatiques, expérimentation).

use crate::amount::COIN;
use crate::crypto::PublicKey;
use crate::hash::Hash256;
use crate::u256::U256;
use std::fmt;

/// Avis affiché par les programmes sur les réseaux de test (voir AVIS.md).
pub const TEST_NETWORK_NOTICE: &str = "Réseau de test : les KYNO n'ont aucune valeur, ne sont pas à vendre et ne \
                                       constituent pas un placement. Aucune vente, aucune prévente, aucune promesse.";

/// Identifiant de réseau.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Network {
    Mainnet,
    Testnet,
    Regtest,
}

impl Network {
    /// Tous les réseaux connus.
    pub const ALL: [Network; 3] = [Network::Mainnet, Network::Testnet, Network::Regtest];

    /// Nom court (« mainnet », « testnet », « regtest »).
    pub fn name(self) -> &'static str {
        match self {
            Network::Mainnet => "mainnet",
            Network::Testnet => "testnet",
            Network::Regtest => "regtest",
        }
    }

    /// Lit un nom de réseau.
    pub fn from_name(s: &str) -> Option<Network> {
        Network::ALL.into_iter().find(|n| n.name() == s)
    }

    /// Paramètres de consensus du réseau.
    pub fn params(self) -> &'static ConsensusParams {
        match self {
            Network::Mainnet => &MAINNET,
            Network::Testnet => &TESTNET,
            Network::Regtest => &REGTEST,
        }
    }
}

impl fmt::Display for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Paramètres du bloc de genèse.
#[derive(Debug, Clone)]
pub struct GenesisParams {
    /// Horodatage (secondes Unix).
    pub timestamp: u64,
    /// Cible de difficulté du bloc de genèse, aussi utilisée pour les
    /// premiers blocs tant que la fenêtre LWMA n'est pas remplie.
    pub target: U256,
    /// Nonce trouvé lors du minage de la genèse.
    pub nonce: u64,
    /// Message gravé dans la transaction de récompense (64 octets max).
    pub message: &'static str,
    /// Empreinte attendue (None : réseau pas encore lancé).
    pub hash: Option<Hash256>,
}

/// Paramètres de consensus.
#[derive(Debug, Clone)]
pub struct ConsensusParams {
    pub network: Network,
    /// Préfixe des adresses (provisoire, voir docs/DECISIONS.md).
    pub hrp: &'static str,
    /// Octets magiques en tête de chaque message réseau.
    pub magic: [u8; 4],
    pub default_p2p_port: u16,
    pub default_rpc_port: u16,
    /// Cible la plus facile autorisée (difficulté minimale).
    pub pow_limit: U256,
    pub genesis: GenesisParams,
    /// Temps visé entre deux blocs (secondes).
    pub target_spacing: u64,
    /// Nombre de blocs de la fenêtre LWMA.
    pub lwma_window: u64,
    /// regtest : difficulté fixe (cible = `pow_limit`).
    pub no_retarget: bool,
    /// Un bloc ne peut pas être daté de plus de N secondes dans le futur.
    pub max_future_block_time: u64,
    /// Récompense initiale par bloc (unités).
    pub initial_subsidy: u64,
    /// Nombre de blocs entre deux divisions par deux de la récompense.
    pub halving_interval: u64,
    /// Nombre de blocs avant qu'une récompense de minage soit dépensable.
    pub coinbase_maturity: u64,
    /// Type de pièce SLIP-44 pour la dérivation des clés du portefeuille.
    pub slip44_coin_type: u32,
    /// Clé maîtresse des points de contrôle signés (None : désactivés). Voir
    /// `checkpoint.rs`.
    pub checkpoint_master: Option<PublicKey>,
    /// Plus aucun point de contrôle n'est accepté au-delà de cette hauteur :
    /// le réseau redevient purement fondé sur la preuve de travail.
    pub checkpoint_sunset_height: u64,
    /// Points de contrôle inscrits dans cette version du logiciel (hauteur,
    /// bloc), par hauteur croissante : un nœud qui démarre de zéro refuse dès
    /// le départ toute chaîne qui ne les contient pas, avant même d'avoir reçu
    /// un point signé. Mis à jour à chaque version (« checkpoint export »).
    pub release_checkpoints: &'static [(u64, Hash256)],
    /// Période de rodage (règle de consensus) : aucun transfert (transaction
    /// autre qu'une récompense de minage) n'est valide dans un bloc de hauteur
    /// inférieure. On mine et on accumule, mais personne ne peut envoyer de
    /// pièces avant cette hauteur, fixée d'avance et que personne ne peut avancer.
    pub transfers_unlock_height: u64,
    /// Nœuds d'amorçage (« IP:PORT ») joints au démarrage quand aucun pair
    /// n'est imposé par `--connect` (désactivables avec `--noseed`). Ce n'est
    /// pas une règle de consensus : seulement le premier contact avec le réseau.
    pub seeds: &'static [&'static str],
}

impl ConsensusParams {
    /// Récompense du bloc à la hauteur `height` (règle de consensus).
    pub fn block_subsidy(&self, height: u64) -> u64 {
        let halvings = height / self.halving_interval;
        if halvings >= 64 { 0 } else { self.initial_subsidy >> halvings }
    }

    /// Émission totale (hors bloc de genèse, dont la récompense est inutilisable).
    pub fn total_supply(&self) -> u64 {
        let mut total: u64 = 0;
        for halving in 0..64u64 {
            let subsidy = self.initial_subsidy >> halving;
            if subsidy == 0 {
                break;
            }
            total = total.saturating_add(subsidy.saturating_mul(self.halving_interval));
        }
        total.saturating_sub(self.initial_subsidy)
    }

    /// Réseau de test ou de développement (pièces sans valeur) ?
    pub fn is_test_network(&self) -> bool {
        self.network != Network::Mainnet
    }

    /// Les transferts sont-ils permis dans un bloc de cette hauteur (fin de la
    /// période de rodage) ?
    pub fn transfers_allowed(&self, height: u64) -> bool {
        height >= self.transfers_unlock_height
    }

    /// Bloc imposé à cette hauteur par la version du logiciel, s'il y en a un.
    pub fn release_checkpoint_at(&self, height: u64) -> Option<Hash256> {
        self.release_checkpoints.iter().find(|(h, _)| *h == height).map(|(_, b)| *b)
    }

    /// Le réseau a-t-il un bloc de genèse définitif ?
    pub fn is_launched(&self) -> bool {
        self.genesis.hash.is_some()
    }
}

/// Réseau principal — PAS ENCORE LANCÉ. Les valeurs de genèse sont des
/// espaces réservés : elles seront fixées lors de la cérémonie de lancement.
pub static MAINNET: ConsensusParams = ConsensusParams {
    network: Network::Mainnet,
    hrp: "kyn",
    magic: [0x4b, 0x59, 0x4e, 0x4d],
    default_p2p_port: 9733,
    default_rpc_port: 9734,
    pow_limit: U256::low_bits(232),
    genesis: GenesisParams {
        timestamp: 0,
        target: U256::low_bits(220),
        nonce: 0,
        message: "Kynovara mainnet - a definir lors du lancement",
        hash: None,
    },
    target_spacing: 120,
    lwma_window: 120,
    no_retarget: false,
    // Environ N·T/20 (voir le réseau de test).
    max_future_block_time: 720,
    initial_subsidy: 50 * COIN,
    halving_interval: 1_051_200,
    coinbase_maturity: 100,
    slip44_coin_type: 9733,
    // Décidés lors de la cérémonie de lancement (docs/LANCEMENT.md).
    checkpoint_master: None,
    checkpoint_sunset_height: 0,
    release_checkpoints: &[],
    // Proposition : 30 jours de rodage (à trancher au lancement, DECISIONS.md § 13).
    transfers_unlock_height: 21_600,
    // Fixés lors de la cérémonie de lancement.
    seeds: &[],
};

/// Réseau de test.
pub static TESTNET: ConsensusParams = ConsensusParams {
    network: Network::Testnet,
    hrp: "tkyn",
    magic: [0x4b, 0x59, 0x4e, 0x54],
    default_p2p_port: 19733,
    default_rpc_port: 19734,
    pow_limit: U256::low_bits(240),
    genesis: GenesisParams {
        timestamp: 1_793_275_200,
        target: U256::low_bits(226),
        nonce: 482_146_465,
        message: "Kynovara testnet - 2026-10-29",
        hash: Some(Hash256::from_hex_const("00000000a8fcd29f180f31ecbe6d2b3a6893eabe8b74c4c65abf3257a9258505")),
    },
    target_spacing: 120,
    lwma_window: 60,
    no_retarget: false,
    // Limite recommandée pour LWMA : environ N·T/20. Un horodatage poussé à la
    // limite ne rend alors le bloc suivant qu'environ 10 % plus facile.
    max_future_block_time: 360,
    initial_subsidy: 50 * COIN,
    halving_interval: 1_051_200,
    coinbase_maturity: 100,
    slip44_coin_type: 1,
    // Clé maîtresse du fondateur (créée le 29 octobre 2026 avec
    // « kynovara-cli checkpoint master-new »). Fin : environ 2 ans de blocs.
    checkpoint_master: Some(PublicKey(
        Hash256::from_hex_const("c74fc55981208da99ae742604ed4963a4de89d8dd21d0559f0943521114ef6be").0,
    )),
    checkpoint_sunset_height: 525_600,
    // Derniers points signés au moment des versions 0.2.1 et 0.3.2.
    release_checkpoints: &[
        (112, Hash256::from_hex_const("00000001b6bf7cd80e53e26f5d83c6d5d39b76f1dd7a95b95ca4991452bf4816")),
        (1398, Hash256::from_hex_const("000000016b79b2e1239187f81b54d0da887b9c0d82be96c33795a23d6ad9071b")),
    ],
    // Rodage d'essai décidé le 30 octobre 2026 (hauteur alors 1 048) : envois
    // possibles à partir du bloc 6 000, soit environ une semaine.
    transfers_unlock_height: 6_000,
    // Serveur public du fondateur (OVH, Beauharnois, Québec), en service
    // depuis le 31 octobre 2026.
    seeds: &["51.222.143.109:19733"],
};

/// Secret **public** de la clé maîtresse des points de contrôle du regtest
/// (tests et développement uniquement, jamais pour un vrai réseau). Il est
/// dérivé de la phrase publique « abandon » × 23 puis « art ».
pub const REGTEST_CHECKPOINT_SECRET: [u8; 32] =
    Hash256::from_hex_const("036d27b66e0ccb28c9abc08a68db375d5a8e8721df23898ed96ef55175f440fd").0;

/// Réseau de développement local (blocs instantanés).
pub static REGTEST: ConsensusParams = ConsensusParams {
    network: Network::Regtest,
    hrp: "rkyn",
    magic: [0x4b, 0x59, 0x4e, 0x52],
    default_p2p_port: 29733,
    default_rpc_port: 29734,
    pow_limit: U256::low_bits(255),
    genesis: GenesisParams {
        timestamp: 1_767_225_600,
        target: U256::low_bits(255),
        nonce: 0,
        message: "Kynovara regtest",
        hash: Some(Hash256::from_hex_const("2e4129d99337d15ae4008f8138d0c3c7da5aee4d93f99894796ef77548a649a6")),
    },
    target_spacing: 120,
    lwma_window: 60,
    no_retarget: true,
    max_future_block_time: 86_400,
    initial_subsidy: 50 * COIN,
    halving_interval: 150,
    coinbase_maturity: 100,
    slip44_coin_type: 1,
    // Clé de développement dont le secret est public (REGTEST_CHECKPOINT_SECRET).
    checkpoint_master: Some(PublicKey(
        Hash256::from_hex_const("9533fcc53a4f515ef6e668d258af74b7adc602a23288aebb728220b3898dbb1f").0,
    )),
    checkpoint_sunset_height: u64::MAX,
    release_checkpoints: &[],
    transfers_unlock_height: 0,
    seeds: &[],
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amount::MAX_MONEY;

    #[test]
    fn seeds_are_valid_public_addresses() {
        for net in Network::ALL {
            let p = net.params();
            for seed in p.seeds {
                let addr: std::net::SocketAddr = seed.parse().unwrap_or_else(|_| panic!("{net} : « {seed} »"));
                assert!(!addr.ip().is_loopback() && !addr.ip().is_unspecified(), "{net} : {seed}");
                assert_eq!(addr.port(), p.default_p2p_port, "{net} : {seed}");
            }
        }
        assert!(MAINNET.seeds.is_empty(), "fixés seulement au lancement");
        assert!(!TESTNET.seeds.is_empty());
    }

    #[test]
    fn subsidy_schedule() {
        let p = &MAINNET;
        assert_eq!(p.block_subsidy(0), 50 * COIN);
        assert_eq!(p.block_subsidy(p.halving_interval - 1), 50 * COIN);
        assert_eq!(p.block_subsidy(p.halving_interval), 25 * COIN);
        assert_eq!(p.block_subsidy(p.halving_interval * 2), 25 * COIN / 2);
        assert_eq!(p.block_subsidy(p.halving_interval * 64), 0);
        assert_eq!(p.block_subsidy(u64::MAX), 0);
    }

    #[test]
    fn total_supply_below_max_money() {
        for net in Network::ALL {
            let p = net.params();
            assert!(p.total_supply() <= MAX_MONEY, "{net}");
        }
        // Environ 105 millions de KYNO sur le réseau principal.
        let total = MAINNET.total_supply();
        assert!(total > 105_000_000 * COIN && total < 105_200_000 * COIN, "{total}");
    }

    #[test]
    fn network_names_and_distinct_magic() {
        for net in Network::ALL {
            assert_eq!(Network::from_name(net.name()), Some(net));
        }
        assert_eq!(Network::from_name("autre"), None);
        assert_ne!(MAINNET.magic, TESTNET.magic);
        assert_ne!(TESTNET.magic, REGTEST.magic);
        assert_ne!(MAINNET.hrp, TESTNET.hrp);
        assert!(!MAINNET.is_launched());
    }

    #[test]
    fn retarget_parameters_are_usable() {
        // Une fenêtre ou un temps de bloc nul provoquerait une division par zéro.
        for net in Network::ALL {
            let p = net.params();
            assert!(p.target_spacing > 0 && p.halving_interval > 0, "{net}");
            if !p.no_retarget {
                assert!(p.lwma_window > 0, "{net}");
            }
            assert!(!p.pow_limit.is_zero() && p.genesis.target <= p.pow_limit, "{net}");
        }
    }

    #[test]
    fn transfer_lock_ends_before_checkpoints_do() {
        // Le rodage doit finir pendant que le fondateur protège encore la
        // chaîne, et le regtest ne doit jamais bloquer les envois.
        assert!(TESTNET.transfers_unlock_height <= TESTNET.checkpoint_sunset_height);
        assert!(REGTEST.transfers_allowed(1));
        assert!(!TESTNET.transfers_allowed(5_999));
        assert!(TESTNET.transfers_allowed(6_000));
    }

    #[test]
    fn checkpoint_master_keys_are_usable() {
        use crate::checkpoint::Checkpoint;
        use std::collections::HashSet;
        // Une signature fausse est refusée proprement (et non par une clé
        // illisible) : la clé publique inscrite est un point Ed25519 valide.
        for p in [&REGTEST, &TESTNET] {
            let key = p.checkpoint_master.expect("clé maîtresse inscrite");
            assert!(crate::crypto::is_valid_public_key(&key), "{}", p.network);
            let cp = Checkpoint {
                height: 1,
                block: Hash256::ZERO,
                authority: crate::checkpoint::Authority::Master,
                signature: crate::crypto::Signature([0; 64]),
            };
            assert!(cp.verify(p, &Hash256::ZERO, &HashSet::new()).is_err());
        }
    }

    #[test]
    fn future_time_limit_suits_lwma() {
        // Un horodatage avancé au maximum ne doit pas rendre le bloc suivant
        // plus de ~10 % plus facile : FTL ≤ N·T/20 (recommandation LWMA).
        for p in [&MAINNET, &TESTNET] {
            assert!(p.max_future_block_time * 20 <= p.lwma_window * p.target_spacing, "{}", p.network);
            // …et reste bien au-delà d'un décalage d'horloge ordinaire.
            assert!(p.max_future_block_time >= 300);
        }
    }

    #[test]
    fn genesis_messages_fit() {
        for net in Network::ALL {
            let m = net.params().genesis.message;
            assert!(m.len() <= crate::tx::MAX_COINBASE_EXTRA && m.is_ascii());
        }
    }
}
