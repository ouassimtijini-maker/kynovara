//! Carnet d'adresses des pairs connus (découverte du réseau).
//!
//! Protections :
//! - une adresse reçue d'un pair n'est retenue que si elle est **routable sur
//!   Internet** (pas de boucle locale, d'adresse privée ou de lien local : un
//!   pair ne doit pas pouvoir faire contacter par le nœud un service interne).
//!   Exception : entre deux machines d'un même réseau privé, les adresses
//!   privées sont acceptées ;
//! - une même source (adresse IPv4, ou préfixe IPv6 /64) ne peut fournir plus
//!   de `MAX_PER_SOURCE` adresses par période de 24 h, même en se
//!   reconnectant ;
//! - quand le carnet est plein, une adresse jamais jointe avec succès est
//!   évincée de préférence à une adresse « éprouvée ».

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::time::{Duration, Instant};

const MAX_ENTRIES: usize = 2_000;
/// Délai minimal entre deux tentatives vers la même adresse.
const RETRY_DELAY: Duration = Duration::from_secs(60);
/// Adresses acceptées au plus d'une même source par période.
pub const MAX_PER_SOURCE: u32 = 1_000;
const SOURCE_WINDOW: Duration = Duration::from_secs(24 * 3600);
const MAX_SOURCES: usize = 4_096;

#[derive(Clone, Debug, Default)]
struct Entry {
    last_attempt: Option<Instant>,
    failures: u32,
    success: bool,
}

/// Carnet d'adresses borné.
#[derive(Debug)]
pub struct AddrBook {
    entries: HashMap<SocketAddr, Entry>,
    /// Adresses fournies par chaque source : (nombre, début de la période).
    sources: HashMap<IpAddr, (u32, Instant)>,
    rng: u64,
}

/// L'adresse peut-elle désigner un pair ?
pub fn is_usable(addr: &SocketAddr) -> bool {
    addr.port() != 0 && !addr.ip().is_unspecified() && !addr.ip().is_multicast()
}

/// Adresse privée ou locale à un réseau (IPv4 privée ou lien local, IPv6
/// locale unique fc00::/7 ou lien local fe80::/10).
fn is_private(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

/// Adresse joignable sur Internet (unicast publique).
pub fn is_routable(ip: &IpAddr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() || is_private(ip) {
        return false;
    }
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            // Diffusion, documentation, partage d'opérateur (100.64.0.0/10), réservé (240.0.0.0/4).
            !(v4.is_broadcast()
                || v4.is_documentation()
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
                || o[0] >= 240
                || o[0] == 0)
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            // Seul l'espace unicast global 2000::/3 est routable, moins la
            // documentation 2001:db8::/32.
            let global_unicast = (s[0] & 0xe000) == 0x2000;
            let documentation = s[0] == 0x2001 && s[1] == 0x0db8;
            global_unicast && !documentation
        }
    }
}

/// Une adresse annoncée par `source` peut-elle être retenue ?
pub fn accepts_from(addr: &SocketAddr, source: &IpAddr) -> bool {
    is_usable(addr) && (is_routable(&addr.ip()) || (is_private(&addr.ip()) && is_private(source)))
}

/// Clé d'une source ou d'un bannissement : l'adresse IPv4, ou le préfixe /64
/// d'une adresse IPv6 (un abonné dispose d'au moins un /64 entier).
pub fn ip_key(ip: &IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => *ip,
        IpAddr::V6(v6) => {
            let s = v6.segments();
            IpAddr::V6(Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
        }
    }
}

impl AddrBook {
    pub fn new(seed: u64) -> Self {
        AddrBook { entries: HashMap::new(), sources: HashMap::new(), rng: seed | 1 }
    }

    fn next_random(&mut self) -> u64 {
        // xorshift64 : suffisant pour choisir des pairs au hasard.
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Ajoute une adresse de confiance (fichier local, configuration).
    pub fn add(&mut self, addr: SocketAddr) {
        if !is_usable(&addr) || self.entries.contains_key(&addr) {
            return;
        }
        if self.entries.len() >= MAX_ENTRIES {
            // Évince d'abord une adresse jamais jointe avec succès.
            let unproven: Vec<SocketAddr> = self.entries.iter().filter(|(_, e)| !e.success).map(|(a, _)| *a).collect();
            let pool: Vec<SocketAddr> =
                if unproven.is_empty() { self.entries.keys().copied().collect() } else { unproven };
            let victim = pool[(self.next_random() as usize) % pool.len()];
            self.entries.remove(&victim);
        }
        self.entries.insert(addr, Entry::default());
    }

    /// Ajoute une adresse annoncée par un pair d'adresse `source`. Renvoie
    /// `false` si elle est refusée (non routable, ou quota de la source
    /// atteint).
    pub fn add_from(&mut self, addr: SocketAddr, source: IpAddr) -> bool {
        if !accepts_from(&addr, &source) {
            return false;
        }
        let key = ip_key(&source);
        let now = Instant::now();
        if !self.sources.contains_key(&key) && self.sources.len() >= MAX_SOURCES {
            // Oublie la source dont la période est la plus ancienne.
            if let Some(oldest) = self.sources.iter().min_by_key(|(_, (_, since))| *since).map(|(k, _)| *k) {
                self.sources.remove(&oldest);
            }
        }
        let (count, since) = self.sources.entry(key).or_insert((0, now));
        if now.duration_since(*since) > SOURCE_WINDOW {
            *count = 0;
            *since = now;
        }
        if *count >= MAX_PER_SOURCE {
            return false;
        }
        *count += 1;
        self.add(addr);
        true
    }

    /// Oublie une adresse.
    pub fn remove(&mut self, addr: &SocketAddr) {
        self.entries.remove(addr);
    }

    pub fn mark_attempt(&mut self, addr: &SocketAddr) {
        if let Some(e) = self.entries.get_mut(addr) {
            e.last_attempt = Some(Instant::now());
        }
    }

    pub fn mark_success(&mut self, addr: &SocketAddr) {
        if let Some(e) = self.entries.get_mut(addr) {
            e.success = true;
            e.failures = 0;
        }
    }

    pub fn mark_failure(&mut self, addr: &SocketAddr) {
        if let Some(e) = self.entries.get_mut(addr) {
            e.failures = e.failures.saturating_add(1);
            // Trop d'échecs sans jamais réussir : on oublie l'adresse.
            if e.failures >= 10 && !e.success {
                self.entries.remove(addr);
            }
        }
    }

    /// Choisit une adresse à essayer, hors `exclude`.
    pub fn pick(&mut self, exclude: &HashSet<SocketAddr>) -> Option<SocketAddr> {
        let now = Instant::now();
        let candidates: Vec<SocketAddr> = self
            .entries
            .iter()
            .filter(|(a, e)| {
                !exclude.contains(a) && e.last_attempt.is_none_or(|t| now.duration_since(t) >= RETRY_DELAY)
            })
            .map(|(a, _)| *a)
            .collect();
        if candidates.is_empty() {
            return None;
        }
        let i = (self.next_random() as usize) % candidates.len();
        Some(candidates[i])
    }

    /// Échantillon aléatoire (réponse à `GetAddr`).
    pub fn sample(&mut self, max: usize) -> Vec<SocketAddr> {
        let mut all: Vec<SocketAddr> = self.entries.keys().copied().collect();
        // Mélange de Fisher-Yates.
        for i in (1..all.len()).rev() {
            let j = (self.next_random() as usize) % (i + 1);
            all.swap(i, j);
        }
        all.truncate(max);
        all
    }

    /// Enregistre les adresses (une par ligne). Au mieux : une erreur est ignorée.
    pub fn save(&self, path: &Path) {
        let text: String = self.entries.keys().map(|a| format!("{a}\n")).collect();
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    /// Recharge les adresses enregistrées.
    pub fn load(&mut self, path: &Path) {
        if let Ok(text) = std::fs::read_to_string(path) {
            for line in text.lines().take(MAX_ENTRIES) {
                if let Ok(addr) = line.trim().parse() {
                    self.add(addr);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(i: u16) -> SocketAddr {
        SocketAddr::from(([8, 8, (i >> 8) as u8, i as u8], 19733))
    }

    fn src(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn add_pick_and_bounds() {
        let mut book = AddrBook::new(7);
        book.add("0.0.0.0:1".parse().unwrap());
        book.add("10.0.0.1:0".parse().unwrap());
        assert!(book.is_empty());
        for i in 0..3000 {
            book.add(addr(i));
        }
        assert_eq!(book.len(), MAX_ENTRIES);
        let exclude: HashSet<SocketAddr> = HashSet::new();
        let a = book.pick(&exclude).unwrap();
        book.mark_attempt(&a);
        assert_eq!(book.sample(10).len(), 10);
    }

    #[test]
    fn failures_are_forgotten() {
        let mut book = AddrBook::new(1);
        book.add(addr(1));
        for _ in 0..10 {
            book.mark_failure(&addr(1));
        }
        assert!(book.is_empty());
    }

    #[test]
    fn retry_delay_respected() {
        let mut book = AddrBook::new(3);
        book.add(addr(5));
        book.mark_attempt(&addr(5));
        assert_eq!(book.pick(&HashSet::new()), None);
    }

    #[test]
    fn routability() {
        for public in ["8.8.8.8", "1.2.3.4", "2606:4700::1111", "2001:4860::8888"] {
            assert!(is_routable(&src(public)), "{public}");
        }
        for internal in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.1.20",
            "172.16.0.1",
            "169.254.1.1",
            "100.64.0.1",
            "0.1.2.3",
            "255.255.255.255",
            "192.0.2.1",
            "240.0.0.1",
            "::1",
            "fd00::1",
            "fe80::1",
            "2001:db8::1",
            "ff02::1",
        ] {
            assert!(!is_routable(&src(internal)), "{internal}");
        }
    }

    /// Non-régression (audit M-4).
    #[test]
    fn network_supplied_addresses_are_filtered_and_rate_limited() {
        let mut book = AddrBook::new(9);
        let internet = src("8.8.4.4");
        // Une adresse de boucle locale ou privée annoncée depuis Internet est refusée…
        assert!(!book.add_from("127.0.0.1:19733".parse().unwrap(), internet));
        assert!(!book.add_from("192.168.1.5:19733".parse().unwrap(), internet));
        // … mais deux machines d'un même réseau privé peuvent se les échanger.
        assert!(book.add_from("192.168.1.5:19733".parse().unwrap(), src("192.168.1.20")));
        // Jamais de boucle locale, même entre pairs locaux.
        assert!(!book.add_from("127.0.0.1:19733".parse().unwrap(), src("127.0.0.1")));
        // Quota par source, même en se reconnectant (la source est l'adresse IP).
        let mut accepted = 0;
        for i in 0..3000u16 {
            if book.add_from(addr(i), internet) {
                accepted += 1;
            }
        }
        assert_eq!(accepted, MAX_PER_SOURCE);
        // Une IPv6 du même /64 compte comme la même source.
        let v6a: IpAddr = src("2606:4700:10:20::1");
        let v6b: IpAddr = src("2606:4700:10:20:ffff::2");
        assert_eq!(ip_key(&v6a), ip_key(&v6b));
        for i in 0..MAX_PER_SOURCE as u16 {
            book.add_from(SocketAddr::from(([9, 9, (i >> 8) as u8, i as u8], 1)), v6a);
        }
        assert!(!book.add_from(SocketAddr::from(([9, 10, 0, 0], 1)), v6b));
    }

    #[test]
    fn proven_addresses_survive_a_flood() {
        let mut book = AddrBook::new(5);
        let good = SocketAddr::from(([7, 7, 7, 7], 19733));
        book.add(good);
        book.mark_success(&good);
        for i in 0..5_000u16 {
            book.add(SocketAddr::from(([9, (i >> 8) as u8, i as u8, 1], 19733)));
        }
        assert_eq!(book.len(), MAX_ENTRIES);
        assert!(book.entries.contains_key(&good), "une adresse éprouvée n'est pas évincée par des inconnues");
    }
}
