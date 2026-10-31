//! Saisies au clavier : mots de passe (masqués), confirmations, phrases.
//!
//! Avec `--password-stdin`, toutes les saisies sont lues ligne par ligne sur
//! l'entrée standard (scripts et tests automatiques).

use std::io::{BufRead, Write};
use zeroize::Zeroizing;

/// Source des saisies.
#[derive(Clone, Copy, Debug)]
pub struct Prompter {
    pub from_stdin: bool,
}

impl Prompter {
    fn read_stdin_line(&self) -> Result<Zeroizing<String>, String> {
        // Capacité réservée : une phrase de 24 mots ou un mot de passe tiennent
        // sans réallocation (qui laisserait une copie dans la mémoire libérée).
        let mut line = Zeroizing::new(String::with_capacity(1024));
        let n = std::io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|e| format!("lecture de l'entrée standard impossible : {e}"))?;
        if n == 0 {
            return Err("entrée standard terminée : saisie manquante".into());
        }
        let trimmed = Zeroizing::new(line.trim_end_matches(['\r', '\n']).to_string());
        Ok(trimmed)
    }

    /// Mot de passe (masqué à l'écran).
    pub fn password(&self, prompt: &str) -> Result<Zeroizing<String>, String> {
        if self.from_stdin {
            eprint!("{prompt}");
            let _ = std::io::stderr().flush();
            let p = self.read_stdin_line()?;
            eprintln!();
            return Ok(p);
        }
        rpassword::prompt_password(prompt)
            .map(Zeroizing::new)
            .map_err(|e| format!("saisie du mot de passe impossible : {e}"))
    }

    /// Ligne visible.
    pub fn line(&self, prompt: &str) -> Result<Zeroizing<String>, String> {
        eprint!("{prompt}");
        let _ = std::io::stderr().flush();
        self.read_stdin_line()
    }

    /// Question oui/non (non par défaut).
    pub fn confirm(&self, prompt: &str) -> Result<bool, String> {
        let answer = self.line(&format!("{prompt} [o/N] : "))?;
        Ok(matches!(answer.trim().to_lowercase().as_str(), "o" | "oui" | "y" | "yes"))
    }
}
