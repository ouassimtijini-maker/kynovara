<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="logo/kyno-logo-sombre.svg">
    <img src="logo/kyno-logo.svg" alt="KYNO · Kynovara" width="420">
  </picture>
</p>

# Kynovara

**Kynovara** est une cryptomonnaie à preuve de travail écrite en Rust, de zéro :
son propre réseau, son propre bloc de genèse, son propre minage.

> ⚠️ **Réseau de test : les KYNO n'ont aucune valeur. Aucune vente, aucune
> prévente, aucun investissement, aucune promesse.** Lis [AVIS.md](AVIS.md).

> **État du projet : version 0.3.2, réseau de test public ouvert depuis le
> 31 octobre 2026.** Le réseau principal (« mainnet ») n'est pas lancé. Avant
> tout lancement public avec de la vraie valeur, lis
> [docs/LANCEMENT.md](docs/LANCEMENT.md) et [AUDIT.md](AUDIT.md).

## Rejoindre le réseau de test

Avec Docker, en trois commandes (détails : [docs/REJOINDRE.md](docs/REJOINDRE.md)) :

```sh
docker build -t kynovara:0.3.2 .
docker run -d --name kynovara --restart unless-stopped --cpus 2 -v kynovara:/data kynovara:0.3.2
docker exec kynovara kynovara-cli --datadir /data info
```

Le nœud trouve seul le serveur public `51.222.143.109:19733` ; aucun port à
ouvrir. Les envois sont bloqués jusqu'au bloc 6 000 (période de rodage).

## Ce qu'il y a dedans

| Programme | Rôle |
|---|---|
| `kynovarad` | Le **nœud** : stocke la chaîne, se connecte aux autres nœuds, valide et relaie blocs et transactions, **mine**, et expose une interface de commande locale (RPC). |
| `kynovara-cli` | Le **portefeuille** et l'outil de commande : phrase de récupération de 24 mots, adresses, solde, envois, pilotage du nœud et du minage. |

En bref, techniquement :

- **Preuve de travail** SHA-256 double (comme Bitcoin), bloc toutes les **2 minutes** en moyenne ;
- **ajustement de difficulté LWMA** à chaque bloc (réagit vite aux changements de puissance de calcul) ;
- **modèle UTXO** (comme Bitcoin), signatures **Ed25519** non malléables ;
- **émission** : 50 KYNO par bloc, divisée par deux environ tous les 4 ans, environ **105 millions** de KYNO au total, **aucune pré-extraction** ;
- adresses **Bech32m** (`kyn1…`, `tkyn1…` en test) qui détectent les fautes de frappe ;
- portefeuille déterministe (**BIP-39** + **SLIP-0010**), fichier chiffré **Argon2id + XChaCha20-Poly1305** ;
- réseau pair-à-pair maison (TCP), synchronisation, relais, bannissement des pairs abusifs ;
- **période de rodage** : pendant un temps fixé d'avance, on mine mais aucun envoi n'est possible ; ensuite, crypto normale, automatiquement ;
- **points de contrôle signés** par le fondateur pendant le lancement : un attaquant majoritaire ne peut plus réécrire l'historique signé (clé maîtresse hors ligne, alerte en cas de contradiction, fin programmée) ;
- **aucune ligne de code `unsafe`**, dépendances limitées à des bibliothèques reconnues.

Le nom **Kynovara**, le ticker **KYNO** et les préfixes d'adresse
**`kyn` / `tkyn` / `rkyn`** sont choisis ; la vérification de marque et l'avis
juridique restent à faire avant le lancement : voir [docs/DECISIONS.md](docs/DECISIONS.md).

## Les trois réseaux

| Réseau | À quoi il sert | Ports (P2P / RPC) |
|---|---|---|
| `regtest` | Découverte et tests : les blocs se minent **instantanément** sur commande. | 29733 / 29734 |
| `testnet` | Réseau de test avec une **vraie difficulté** : pour miner sur ton PC, seul ou avec d'autres machines. | 19733 / 19734 |
| `mainnet` | Le futur réseau principal. **Pas encore lancé** : le nœud refuse de démarrer dessus. | 9733 / 9734 |

## Par où commencer

0. **Rejoindre le réseau de test** (Docker) : [docs/REJOINDRE.md](docs/REJOINDRE.md)
1. **Installer et compiler** : [docs/INSTALLATION-WINDOWS.md](docs/INSTALLATION-WINDOWS.md)
2. **Premiers pas** (créer un portefeuille, miner, envoyer) : [docs/DEMARRAGE.md](docs/DEMARRAGE.md)
3. **Comprendre les règles** : [docs/SPECIFICATION.md](docs/SPECIFICATION.md)
4. **Interface RPC** (pour tes propres outils) : [docs/RPC.md](docs/RPC.md)
5. **Décisions à prendre avant le lancement** : [docs/DECISIONS.md](docs/DECISIONS.md)
6. **Feuille de route vers le réseau principal** : [docs/LANCEMENT.md](docs/LANCEMENT.md)
7. **Rapport d'audit** : [AUDIT.md](AUDIT.md)

## Organisation du code

```
kynovara/
├── crates/
│   ├── kynovara-core/     règles de consensus (aucun accès réseau ni disque)
│   │   └── src/           types, encodage, crypto, PoW, difficulté, validation,
│   │                      état UTXO, réorganisations, mempool, minage
│   ├── kynovara-node/     le nœud kynovarad (P2P, stockage, mineur, RPC)
│   └── kynovara-wallet/   le portefeuille et kynovara-cli
├── docs/                  documentation (en français)
├── AUDIT.md               rapport d'audit
└── rust-toolchain.toml    version exacte de Rust utilisée (installée automatiquement)
```

## Tests

```
cargo test --workspace
```

La suite (190 tests) couvre les vecteurs officiels (RFC 8032, BIP-39,
BIP-350, SLIP-0010, FIPS 180), les règles de consensus, les réorganisations,
un test différentiel de la chaîne, des tests aléatoires et adverses (faux
pairs malveillants, 300 000 entrées altérées), des pannes de disque simulées,
plusieurs vrais nœuds qui se synchronisent en réseau, et un scénario complet
avec le vrai binaire `kynovara-cli`.

## Sécurité, en deux phrases

- La **phrase de 24 mots** est la seule sauvegarde qui compte : écris-la sur
  papier, ne la photographie pas, ne la tape jamais sur un site web.
- Ne donne jamais ton fichier `.cookie` ni ton `wallet.json` + mot de passe à
  quelqu'un (ni à un outil en ligne).

## Licence

Au choix de l'utilisateur : licence **MIT** ([LICENSE-MIT](LICENSE-MIT)) ou
**Apache 2.0** ([LICENSE-APACHE](LICENSE-APACHE)), comme Rust. Toutes deux
autorisent la réutilisation du code et le fournissent **sans aucune
garantie**. Les bibliothèques utilisées sont sous licences MIT, Apache-2.0 ou
BSD-3-Clause, compatibles.
