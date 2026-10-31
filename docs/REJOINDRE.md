# Rejoindre le réseau de test Kynovara

> ⚠️ **Réseau de test : les KYNO n'ont aucune valeur. Aucune vente, aucune
> prévente, aucun investissement, aucune promesse.** Lis [AVIS.md](../AVIS.md).

Il te faut un ordinateur sous Windows, macOS ou Linux, **Docker** et environ
2 Go d'espace disque. **Aucun port à ouvrir** : ton nœud contacte lui-même le
serveur public et n'accepte, par défaut, aucune connexion venue d'Internet.

| | |
|---|---|
| Serveur public (nœud d'amorçage) | `51.222.143.109:19733` (inscrit dans le logiciel depuis la version 0.3.2) |
| Bloc de genèse | `00000000a8fcd29f180f31ecbe6d2b3a6893eabe8b74c4c65abf3257a9258505` |
| Adresses | `tkyn1…` |
| Envois | possibles à partir du bloc 6 000 (vers le 7 novembre 2026) |

Installer Docker : [Docker Desktop](https://www.docker.com/products/docker-desktop/)
sous Windows ou macOS ; sous Ubuntu ou Debian, `sudo apt install docker.io`
(puis ajoute `sudo` devant chaque commande `docker`).

## 1. Télécharger et vérifier

Télécharge `kynovara-0.3.2.zip` et compare son empreinte SHA-256 à celle qui
est publiée à côté :

```powershell
Get-FileHash .\kynovara-0.3.2.zip          # Windows (PowerShell)
```

```sh
shasum -a 256 kynovara-0.3.2.zip           # macOS ou Linux
```

Décompresse l'archive : tu obtiens le dossier `kynovara-0.3.2`.

## 2. Construire l'image (une fois, 10 à 20 minutes)

```sh
cd kynovara-0.3.2
docker build -t kynovara:0.3.2 .
```

La construction compile le code source et relance **tous les tests** du
projet ; elle s'arrête si un seul échoue.

## 3. Démarrer le nœud

```sh
docker run -d --name kynovara --restart unless-stopped --cpus 2 -v kynovara:/data kynovara:0.3.2
```

Le nœud rejoint le serveur public et télécharge la chaîne. `--cpus 2` le limite
à deux cœurs (minage compris). Pour voir où il en est :

```sh
docker exec kynovara kynovara-cli --datadir /data info
```

## 4. Créer ton portefeuille

```sh
docker exec -it kynovara kynovara-cli --datadir /data wallet create
```

**Note les 24 mots sur papier** : eux seuls permettent de recréer le
portefeuille (`wallet restore`). Le mot de passe ne s'affiche pas pendant que
tu le tapes, c'est normal. Copie l'adresse `tkyn1…` affichée.

## 5. Miner (facultatif)

Recrée le conteneur avec ton adresse ; la chaîne et le portefeuille restent
dans le volume `kynovara` :

```sh
docker rm -f kynovara
docker run -d --name kynovara --restart unless-stopped --cpus 2 -v kynovara:/data kynovara:0.3.2 --mine --mining-address <ton-adresse-tkyn1> --threads 2
```

Le minage reprend tout seul après un redémarrage de l'ordinateur. Une
récompense de minage devient dépensable 100 blocs plus tard (environ 3 h 20).

## 6. Suivre

```sh
docker exec kynovara kynovara-cli --datadir /data info                 # hauteur, pairs, minage
docker exec -it kynovara kynovara-cli --datadir /data wallet balance   # solde
docker logs -f kynovara                                                # journal du nœud
```

## Bon à savoir

- **Période de rodage** : jusqu'au bloc 6 000, on mine mais aucun envoi n'est
  accepté (règle de consensus fixée d'avance). Ensuite, les envois
  fonctionnent normalement : `wallet send <adresse> <montant>`.
- **Points de contrôle signés** : pendant le lancement, le fondateur signe les
  nouveaux blocs avec une clé déléguée par sa clé maîtresse
  (`c74fc55981208da99ae742604ed4963a4de89d8dd21d0559f0943521114ef6be`). Ton
  nœud refuse toute chaîne qui contredit ces signatures : une puissance de
  calcul majoritaire ne peut pas réécrire l'historique signé. Ce mécanisme
  s'arrête définitivement au bloc 525 600 ; voir la
  [spécification](SPECIFICATION.md), § 14.
- **Mettre à jour** : construis la nouvelle image (`docker build -t
  kynovara:<version> .`), puis `docker rm -f kynovara` et relance `docker run`
  avec la nouvelle image. Le volume garde la chaîne et le portefeuille.
- **Arrêter / reprendre** : `docker stop kynovara`, puis `docker start kynovara`.
- **Tout effacer** : `docker rm -f kynovara` puis `docker volume rm kynovara`.
  Cela efface aussi le portefeuille : garde tes 24 mots.
- **Sans Docker** : `cargo build --release` (Rust 1.97) produit `kynovarad`
  et `kynovara-cli` ; voir [DEMARRAGE.md](DEMARRAGE.md). Sous Windows, Smart
  App Control peut bloquer des programmes non signés : Docker évite ce
  problème.

---

## Join the Kynovara test network (English)

**Test network: KYNO have no value; nothing is for sale.** With Docker:
download `kynovara-0.3.2.zip`, check its SHA-256, unzip, then
`docker build -t kynovara:0.3.2 .` and
`docker run -d --name kynovara --restart unless-stopped --cpus 2 -v kynovara:/data kynovara:0.3.2`.
The node finds the public seed `51.222.143.109:19733` by itself; no port
needs to be opened. Create a wallet with
`docker exec -it kynovara kynovara-cli --datadir /data wallet create`, and to
mine, recreate the container with
`--mine --mining-address <your-tkyn1-address> --threads 2`. Transfers are
locked until block 6,000 (around November 7, 2026).
