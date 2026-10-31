//! Phrase de récupération BIP-39 (24 mots, liste anglaise officielle).
//!
//! 256 bits d'entropie + 8 bits de contrôle = 24 mots de 11 bits. La graine
//! est dérivée par PBKDF2-HMAC-SHA512 (2048 itérations), comme le prévoit le
//! standard. La phrase de passe BIP-39 optionnelle doit être en ASCII (la
//! normalisation Unicode NFKD est alors l'identité).

use sha2::{Digest, Sha256, Sha512};
use std::fmt;
use std::sync::OnceLock;
use zeroize::Zeroizing;

const WORDLIST_TEXT: &str = include_str!("bip39-english.txt");
/// Nombre de mots de la phrase.
pub const WORD_COUNT: usize = 24;

fn wordlist() -> &'static [&'static str] {
    static LIST: OnceLock<Vec<&'static str>> = OnceLock::new();
    LIST.get_or_init(|| WORDLIST_TEXT.lines().map(str::trim).filter(|w| !w.is_empty()).collect())
}

/// Erreur de phrase de récupération.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bip39Error {
    WrongWordCount(usize),
    UnknownWord { position: usize, word: String },
    BadChecksum,
    NonAsciiPassphrase,
}

impl fmt::Display for Bip39Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Bip39Error::WrongWordCount(n) => write!(f, "la phrase doit contenir {WORD_COUNT} mots ({n} reçus)"),
            Bip39Error::UnknownWord { position, word } => {
                write!(f, "mot n° {position} inconnu : « {word} » (faute de frappe ?)")
            }
            Bip39Error::BadChecksum => write!(f, "phrase invalide : un mot est faux ou l'ordre est incorrect"),
            Bip39Error::NonAsciiPassphrase => write!(f, "la phrase de passe doit être en caractères ASCII"),
        }
    }
}

impl std::error::Error for Bip39Error {}

fn bit(data: &[u8], i: usize) -> u16 {
    u16::from((data[i / 8] >> (7 - i % 8)) & 1)
}

/// Entropie → 24 mots.
pub fn entropy_to_words(entropy: &[u8; 32]) -> Vec<&'static str> {
    let checksum = Sha256::digest(entropy)[0];
    let mut bits = Zeroizing::new([0u8; 33]);
    bits[..32].copy_from_slice(entropy);
    bits[32] = checksum;
    let list = wordlist();
    (0..WORD_COUNT)
        .map(|w| {
            let index = (0..11).fold(0u16, |acc, b| (acc << 1) | bit(&bits[..], w * 11 + b));
            list[usize::from(index)]
        })
        .collect()
}

/// Normalise une saisie : minuscules, espaces simples. Le résultat est effacé
/// de la mémoire à sa libération ; la capacité est réservée d'avance pour
/// qu'aucune réallocation ne laisse de copie de la phrase dans la mémoire
/// libérée (la mise en minuscules peut allonger un texte non ASCII d'au plus
/// la moitié).
pub fn normalize(input: &str) -> Zeroizing<String> {
    let mut out = Zeroizing::new(String::with_capacity(input.len() * 3 / 2 + 1));
    for word in input.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.extend(word.chars().flat_map(char::to_lowercase));
    }
    out
}

/// 24 mots → entropie (vérifie le contrôle).
pub fn words_to_entropy(phrase: &str) -> Result<Zeroizing<[u8; 32]>, Bip39Error> {
    let normalized = normalize(phrase);
    let words: Vec<&str> = normalized.split(' ').filter(|w| !w.is_empty()).collect();
    if words.len() != WORD_COUNT {
        return Err(Bip39Error::WrongWordCount(words.len()));
    }
    let list = wordlist();
    let mut bits = Zeroizing::new([0u8; 33]);
    for (w, word) in words.iter().enumerate() {
        let index = list
            .binary_search(word)
            .map_err(|_| Bip39Error::UnknownWord { position: w + 1, word: (*word).to_string() })?;
        for b in 0..11 {
            if (index >> (10 - b)) & 1 == 1 {
                let i = w * 11 + b;
                bits[i / 8] |= 1 << (7 - i % 8);
            }
        }
    }
    let mut entropy = Zeroizing::new([0u8; 32]);
    entropy.copy_from_slice(&bits[..32]);
    if Sha256::digest(&entropy[..])[0] != bits[32] {
        return Err(Bip39Error::BadChecksum);
    }
    Ok(entropy)
}

/// Graine de 64 octets à partir de la phrase (et d'une phrase de passe ASCII).
pub fn mnemonic_to_seed(words: &[&str], passphrase: &str) -> Result<Zeroizing<[u8; 64]>, Bip39Error> {
    if !passphrase.is_ascii() {
        return Err(Bip39Error::NonAsciiPassphrase);
    }
    Ok(pbkdf2_seed(words, passphrase))
}

/// Graine avec la phrase de passe vide (celle qu'utilise le portefeuille) :
/// aucune erreur possible, donc aucune valeur de repli à inventer.
pub fn mnemonic_to_seed_no_passphrase(words: &[&str]) -> Zeroizing<[u8; 64]> {
    pbkdf2_seed(words, "")
}

fn pbkdf2_seed(words: &[&str], passphrase: &str) -> Zeroizing<[u8; 64]> {
    // `join` calcule la longueur totale et n'alloue qu'une fois : pas de copie
    // résiduelle de la phrase dans la mémoire libérée.
    let mnemonic = Zeroizing::new(words.join(" "));
    let salt = Zeroizing::new(format!("mnemonic{passphrase}"));
    let mut seed = Zeroizing::new([0u8; 64]);
    pbkdf2::pbkdf2_hmac::<Sha512>(mnemonic.as_bytes(), salt.as_bytes(), 2048, &mut seed[..]);
    seed
}

#[cfg(test)]
mod tests {
    use super::*;
    use kynovara_core::hex;

    #[test]
    fn wordlist_is_the_official_one() {
        // Empreinte SHA-256 publiée dans BIP-39 pour la liste anglaise (fins
        // de ligne LF ; une copie convertie en CRLF par Git sous Windows reste
        // reconnue, les mots étant lus sans les fins de ligne).
        let normalized = WORDLIST_TEXT.replace("\r\n", "\n");
        assert_eq!(
            hex::encode(&Sha256::digest(normalized.as_bytes())),
            "2f5eed53a4727b4bf8880d8f3f199efc90e58503646d9ff8eff3a2ed3b24dbda"
        );
        let list = wordlist();
        assert_eq!(list.len(), 2048);
        assert!(list.windows(2).all(|w| w[0] < w[1]), "liste triée");
    }

    // Vecteurs officiels (trezor/python-mnemonic), phrase de passe « TREZOR ».
    const VECTORS: [(&str, &str, &str); 8] = [
        (
            "0000000000000000000000000000000000000000000000000000000000000000",
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art",
            "bda85446c68413707090a52022edd26a1c9462295029f2e60cd7c4f2bbd3097170af7a4d73245cafa9c3cca8d561a7c3de6f5d4a10be8ed2a5e608d68f92fcc8",
        ),
        (
            "7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f",
            "legal winner thank year wave sausage worth useful legal winner thank year wave sausage worth useful legal winner thank year wave sausage worth title",
            "bc09fca1804f7e69da93c2f2028eb238c227f2e9dda30cd63699232578480a4021b146ad717fbb7e451ce9eb835f43620bf5c514db0f8add49f5d121449d3e87",
        ),
        (
            "8080808080808080808080808080808080808080808080808080808080808080",
            "letter advice cage absurd amount doctor acoustic avoid letter advice cage absurd amount doctor acoustic avoid letter advice cage absurd amount doctor acoustic bless",
            "c0c519bd0e91a2ed54357d9d1ebef6f5af218a153624cf4f2da911a0ed8f7a09e2ef61af0aca007096df430022f7a2b6fb91661a9589097069720d015e4e982f",
        ),
        (
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            "zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo vote",
            "dd48c104698c30cfe2b6142103248622fb7bb0ff692eebb00089b32d22484e1613912f0a5b694407be899ffd31ed3992c456cdf60f5d4564b8ba3f05a69890ad",
        ),
        (
            "68a79eaca2324873eacc50cb9c6eca8cc68ea5d936f98787c60c7ebc74e6ce7c",
            "hamster diagram private dutch cause delay private meat slide toddler razor book happy fancy gospel tennis maple dilemma loan word shrug inflict delay length",
            "64c87cde7e12ecf6704ab95bb1408bef047c22db4cc7491c4271d170a1b213d20b385bc1588d9c7b38f1b39d415665b8a9030c9ec653d75e65f847d8fc1fc440",
        ),
        (
            "9f6a2878b2520799a44ef18bc7df394e7061a224d2c33cd015b157d746869863",
            "panda eyebrow bullet gorilla call smoke muffin taste mesh discover soft ostrich alcohol speed nation flash devote level hobby quick inner drive ghost inside",
            "72be8e052fc4919d2adf28d5306b5474b0069df35b02303de8c1729c9538dbb6fc2d731d5f832193cd9fb6aeecbc469594a70e3dd50811b5067f3b88b28c3e8d",
        ),
        (
            "066dca1a2bb7e8a1db2832148ce9933eea0f3ac9548d793112d9a95c9407efad",
            "all hour make first leader extend hole alien behind guard gospel lava path output census museum junior mass reopen famous sing advance salt reform",
            "26e975ec644423f4a4c4f4215ef09b4bd7ef924e85d1d17c4cf3f136c2863cf6df0a475045652c57eb5fb41513ca2a2d67722b77e954b4b3fc11f7590449191d",
        ),
        (
            "f585c11aec520db57dd353c69554b21a89b20fb0650966fa0a9d6f74fd989d8f",
            "void come effort suffer camp survey warrior heavy shoot primary clutch crush open amazing screen patrol group space point ten exist slush involve unfold",
            "01f5bced59dec48e362f2c45b5de68b9fd6c92c6634f44d6d40aab69056506f0e35524a518034ddc1192e1dacd32c1ed3eaa3c3b131c88ed8e7e54c49a5d0998",
        ),
    ];

    #[test]
    fn official_vectors() {
        for (entropy_hex, phrase, seed_hex) in VECTORS {
            let entropy: [u8; 32] = hex::decode_array(entropy_hex).unwrap();
            let words = entropy_to_words(&entropy);
            assert_eq!(words.join(" "), phrase);
            assert_eq!(*words_to_entropy(phrase).unwrap(), entropy);
            let seed = mnemonic_to_seed(&words, "TREZOR").unwrap();
            assert_eq!(hex::encode(&seed[..]), seed_hex);
            assert_eq!(*mnemonic_to_seed_no_passphrase(&words), *mnemonic_to_seed(&words, "").unwrap());
        }
    }

    #[test]
    fn normalize_cleans_spacing_and_case() {
        assert_eq!(normalize("  Abandon\tABOUT \n  zoo ").as_str(), "abandon about zoo");
        assert_eq!(normalize("").as_str(), "");
        // La capacité réservée suffit même pour un texte non ASCII qui s'allonge.
        let s = "İİİİ";
        let n = normalize(s);
        assert!(n.capacity() >= n.len());
    }

    #[test]
    fn input_errors() {
        let phrase = VECTORS[4].1;
        // Majuscules et espaces multiples tolérés.
        let messy = format!("  {}  ", phrase.to_uppercase().replace(' ', "   "));
        assert!(words_to_entropy(&messy).is_ok());
        // Mot manquant.
        let short: Vec<&str> = phrase.split(' ').take(23).collect();
        assert_eq!(words_to_entropy(&short.join(" ")).unwrap_err(), Bip39Error::WrongWordCount(23));
        // Mot inconnu.
        let bad = phrase.replacen("hamster", "hamsterr", 1);
        assert!(matches!(words_to_entropy(&bad), Err(Bip39Error::UnknownWord { position: 1, .. })));
        // Deux mots inversés : contrôle invalide (avec une probabilité écrasante).
        let mut words: Vec<&str> = phrase.split(' ').collect();
        words.swap(0, 1);
        assert_eq!(words_to_entropy(&words.join(" ")).unwrap_err(), Bip39Error::BadChecksum);
        assert_eq!(mnemonic_to_seed(&["abandon"], "é").unwrap_err(), Bip39Error::NonAsciiPassphrase);
    }
}
