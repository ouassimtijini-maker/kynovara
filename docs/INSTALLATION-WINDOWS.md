# Installation sous Windows

Ce guide part d'un PC Windows 10 ou 11 sans outil de développement. Toutes
les commandes se tapent dans **PowerShell** (menu Démarrer → « PowerShell »).

Durée : environ 20 minutes, surtout des téléchargements.

---

## Étape 1 — Installer les outils de compilation C++ de Microsoft

Rust a besoin de l'éditeur de liens de Microsoft (MSVC), et la preuve de
travail RandomX (une bibliothèque C++) du compilateur C++ et de **CMake**.

1. Télécharge **Visual Studio Build Tools** sur le site de Microsoft
   (recherche « Build Tools for Visual Studio »).
2. Dans l'installateur, coche **« Développement Desktop en C++ »**
   (« Desktop development with C++ »). Garde les options par défaut
   (MSVC, le SDK Windows et **« Outils CMake C++ pour Windows »** : vérifie
   que cette case est bien cochée).
3. Lance l'installation (quelques Go).

> Variante : l'installateur de Rust (étape 2) propose aussi d'installer ces
> outils automatiquement. Accepte s'il te le propose.

## Étape 2 — Installer Rust

1. Va sur **https://rustup.rs** et télécharge `rustup-init.exe` (64 bits).
2. Lance-le et choisis l'installation par défaut (touche Entrée).
3. **Ferme PowerShell et rouvre-le** (pour qu'il trouve les nouveaux programmes).

Vérification (une seule commande) :

```powershell
cargo --version
```

Tu dois voir une ligne du genre `cargo 1.xx.x (...)`.

## Étape 3 — Placer le projet

Le projet va dans **ton dossier personnel** (`C:\Users\<ton nom>`), et non à
la racine de `C:\` : par défaut, un dossier créé à la racine du disque est
modifiable par tous les comptes de la machine, qui pourraient alors remplacer
les programmes.

Clique droit sur `kynovara.zip` → **Extraire tout…** → choisis comme
destination `C:\Users\<ton nom>\Kynovara` → **Extraire**. Tu dois avoir
`C:\Users\<ton nom>\Kynovara\Cargo.toml` (et non `…\Kynovara\kynovara\Cargo.toml`).

Puis place-toi dans ce dossier (dans PowerShell, `$HOME` désigne ton dossier
personnel) :

```powershell
cd $HOME\Kynovara
```

## Étape 4 — Installer la version exacte de Rust du projet

Le fichier `rust-toolchain.toml` fixe la version de Rust avec laquelle
Kynovara a été testé. Cette commande l'installe :

```powershell
rustup toolchain install
```

## Étape 5 — Compiler

CMake est installé avec les Build Tools, mais PowerShell ne le trouve pas
tout seul. Indique-lui où il est (une fois par fenêtre PowerShell) :

```powershell
$env:CMAKE = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe"
```

Puis compile :

```powershell
cargo build --release
```

La première fois, cela télécharge les bibliothèques et prend quelques
minutes. Le résultat :

- `$HOME\Kynovara\target\release\kynovarad.exe` — le nœud ;
- `$HOME\Kynovara\target\release\kynovara-cli.exe` — le portefeuille.

> **Important — fichier `Cargo.lock`** : la compilation crée un fichier
> `Cargo.lock` qui fige la version exacte de chaque bibliothèque. Garde-le
> (et ajoute-le à Git) : c'est ce qui rend les compilations reproductibles.

## Étape 6 — Lancer les tests (recommandé)

```powershell
cargo test --workspace --release
```

Tous les tests doivent indiquer `ok`. Le pare-feu Windows peut afficher une
fenêtre pendant les tests réseau : ils n'utilisent que `127.0.0.1` (ta
propre machine) ; tu peux refuser l'accès aux réseaux publics.

## Étape 7 — Raccourcis pratiques (facultatif)

Pour éviter de taper le chemin complet, tu peux copier les deux `.exe` dans
un dossier et l'ajouter au `PATH`, ou simplement te placer dans
`$HOME\Kynovara\target\release` :

```powershell
cd $HOME\Kynovara\target\release
```

Ensuite : `.\kynovarad.exe --help` et `.\kynovara-cli.exe --help`.

---

## Dépannage

**« link.exe not found » ou erreur d'éditeur de liens** : l'étape 1 n'est
pas faite (ou PowerShell n'a pas été rouvert). Installe les Build Tools C++.

**« is `cmake` not installed? » ou erreur de compilation de `randomx-rs`** :
CMake est introuvable. Refais la commande `$env:CMAKE = …` de l'étape 5, et
vérifie dans l'installateur des Build Tools (« Modifier ») que « Outils
CMake C++ pour Windows » est coché.

**« An Application Control policy has blocked this file » (os error 4551)** :
Smart App Control (ou une stratégie de contrôle des applications) bloque les
programmes que la compilation crée et lance elle-même. Ne désactive pas cette
protection pour Kynovara : utilise plutôt Docker
([REJOINDRE.md](REJOINDRE.md)), qui compile et fait tourner le nœud sous
Linux.

**« l'exécution de scripts est désactivée sur ce système »** : tu as lancé
un script `.ps1`. Kynovara n'en a pas besoin ; lance directement les `.exe`.

**L'antivirus bloque `kynovarad.exe`** : certains antivirus se méfient des
programmes qui minent. C'est ton propre programme, compilé sur ta machine ;
tu peux ajouter une exclusion pour `%USERPROFILE%\Kynovara\target`.

**Où sont mes données ?** Dans `%APPDATA%\Kynovara\<réseau>\` :
`blocks\` (la chaîne), `wallet.json` (ton portefeuille), `.cookie` (jeton
RPC, recréé à chaque démarrage), `peers.txt` (pairs connus). Ce dossier de
ton profil n'est lisible que par ton compte Windows (et les administrateurs) :
c'est là que doivent rester le portefeuille et le jeton RPC — n'utilise pas
`--datadir` vers un dossier à la racine de `C:\`. Pour ouvrir le
dossier : touche Windows + R, puis `%APPDATA%\Kynovara`.
