# Premiers pas avec Kynovara

> **Réseau de test : les KYNO n'ont aucune valeur et ne sont pas à vendre.**
> Voir [AVIS.md](../AVIS.md).

Pré-requis : Kynovara compilé (voir [INSTALLATION-WINDOWS.md](INSTALLATION-WINDOWS.md)).

Tu auras besoin de **deux fenêtres PowerShell**, toutes les deux placées dans
le dossier des programmes :

```powershell
cd $HOME\Kynovara\target\release
```

- **Fenêtre 1** : le nœud (il tourne en continu, on le laisse ouvert) ;
- **Fenêtre 2** : les commandes (`kynovara-cli`).

Chaque étape ci-dessous contient **une seule commande** et ce que tu dois
voir. Remplace les valeurs entre `< >` par les tiennes.

---

## Partie A — Découverte en regtest (blocs instantanés)

Le réseau `regtest` est un bac à sable : rien n'y a de valeur et les blocs se
minent sur commande, instantanément.

### A1. Démarrer le nœud (fenêtre 1)

```powershell
.\kynovarad.exe --network regtest
```

Résultat attendu : `réseau regtest : hauteur 0`, puis `Nœud démarré`.
Laisse cette fenêtre ouverte.

### A2. Créer un portefeuille (fenêtre 2)

```powershell
.\kynovara-cli.exe --network regtest wallet create
```

1. Choisis un mot de passe (8 caractères minimum), deux fois.
2. **24 mots** s'affichent : c'est ta sauvegarde (en regtest, tu peux les
   ignorer, mais prends l'habitude de les noter).
3. On te demande deux mots au hasard pour vérifier.
4. La commande affiche ta **première adresse** : `rkyn1…`. Copie-la.

### A3. Miner 101 blocs vers ton adresse

```powershell
.\kynovara-cli.exe --network regtest generate 101 <ton-adresse-rkyn1>
```

Résultat : `101 bloc(s) miné(s).` Chaque bloc rapporte 50 KYNO, mais une
récompense de minage n'est dépensable que **100 blocs plus tard** (maturité).

### A4. Consulter ton solde

```powershell
.\kynovara-cli.exe --network regtest wallet balance
```

Résultat : `Disponible : 100 KYNO` et `En maturation : 4950 KYNO`.

### A5. Créer un second portefeuille (un « ami »)

```powershell
.\kynovara-cli.exe --network regtest --wallet ami.json wallet create
```

Copie l'adresse affichée (`rkyn1…`). Le fichier `ami.json` est créé dans le
dossier courant.

### A6. Envoyer 12,5 KYNO à l'ami

```powershell
.\kynovara-cli.exe --network regtest wallet send <adresse-de-l-ami> 12,5
```

Le portefeuille affiche le montant, les **frais** et la **monnaie rendue**
(la différence revient à une nouvelle adresse à toi), puis demande
confirmation. Réponds `o`. Note l'identifiant de transaction affiché.

### A7. Confirmer la transaction en minant un bloc

```powershell
.\kynovara-cli.exe --network regtest generate 1 <ton-adresse-rkyn1>
```

### A8. Vérifier le solde de l'ami

```powershell
.\kynovara-cli.exe --network regtest --wallet ami.json wallet balance
```

Résultat : `Disponible : 12.5 KYNO`.

### A9. Explorer

Une commande à la fois, au choix :

```powershell
.\kynovara-cli.exe --network regtest info
```

```powershell
.\kynovara-cli.exe --network regtest block 102
```

```powershell
.\kynovara-cli.exe --network regtest tx <identifiant-de-transaction>
```

### A10. Arrêter le nœud

```powershell
.\kynovara-cli.exe --network regtest stop
```

(Ctrl+C dans la fenêtre 1 fonctionne aussi, sans perte de données, mais
`stop` enregistre en plus la liste des pairs connus.)

---

## Partie B — Miner sur le testnet avec ton PC

Le `testnet` a une **vraie difficulté** qui s'ajuste toute seule pour viser un
bloc toutes les 2 minutes. C'est le terrain d'essai avant le réseau principal.

### B1. Créer ton portefeuille testnet (fenêtre 2)

`testnet` est le réseau par défaut : pas besoin de `--network`.

```powershell
.\kynovara-cli.exe wallet create
```

**Cette fois, note les 24 mots sur papier.** Copie l'adresse `tkyn1…`.

### B2. Démarrer le nœud et le minage (fenêtre 1)

```powershell
.\kynovarad.exe --mine --mining-address <ton-adresse-tkyn1>
```

Par défaut, le mineur utilise tous les cœurs du processeur **sauf un** (pour
que ton PC reste utilisable). Pour choisir : ajoute `--threads 8`, par exemple.

Quand un bloc est trouvé, la fenêtre affiche `bloc trouvé à la hauteur …`.

### B3. Suivre le minage (fenêtre 2)

```powershell
.\kynovara-cli.exe info
```

Tu y vois la hauteur, la difficulté, ta puissance de calcul (en MH/s) et le
nombre de blocs trouvés.

> Au début, la difficulté est celle du bloc de genèse (environ un milliard de
> hachages par bloc). Si ton PC trouve les blocs plus vite que toutes les
> 2 minutes, la difficulté monte automatiquement ; plus lentement, elle baisse.

> **Période de rodage du testnet** : jusqu'au bloc 6 000, on mine mais aucun
> envoi n'est accepté (`wallet send` affiche le temps restant). Ensuite, les
> envois fonctionnent normalement.

### B4. Tes gains

```powershell
.\kynovara-cli.exe wallet balance
```

Les récompenses apparaissent d'abord « en maturation » ; elles deviennent
disponibles 100 blocs plus tard (environ 3 h 20 au rythme visé).

**Arrêter / reprendre le minage** sans arrêter le nœud :

```powershell
.\kynovara-cli.exe mining stop
```

```powershell
.\kynovara-cli.exe wallet mine
```

(`wallet mine` reprend le minage vers une nouvelle adresse de ton portefeuille.)

---

## Partie C — Plusieurs nœuds

### C1. Deux nœuds sur le même PC

Le second nœud a besoin de son propre dossier de données (dans ton profil,
comme le premier) et de ses propres ports. Avec le premier nœud testnet déjà
démarré, dans une **troisième** fenêtre :

```powershell
.\kynovarad.exe --datadir "$env:APPDATA\Kynovara-noeud2" --listen 127.0.0.1:19833 --rpc 127.0.0.1:19834 --addnode 127.0.0.1:19733
```

Il se connecte au premier, télécharge la chaîne et suit les nouveaux blocs.
Pour lui parler :

```powershell
.\kynovara-cli.exe --datadir "$env:APPDATA\Kynovara-noeud2" --rpc 127.0.0.1:19834 info
```

### C2. Deux PC sur le même réseau local

Sur le **PC A**, le nœud doit écouter sur le réseau (et pas seulement sur
`127.0.0.1`) :

```powershell
.\kynovarad.exe --listen 0.0.0.0:19733 --mine --mining-address <adresse-tkyn1>
```

Windows demande d'autoriser le programme : accepte pour les **réseaux
privés** seulement. Trouve l'adresse IP du PC A avec `ipconfig`
(ex. `192.168.1.20`).

Sur le **PC B** :

```powershell
.\kynovarad.exe --addnode 192.168.1.20:19733
```

> **Ne jamais exposer le port RPC (19734) sur le réseau.** Il reste sur
> `127.0.0.1`, c'est voulu : il permet de piloter le nœud et le minage.

---

## Partie D — Points de contrôle (fondateur, avec Docker)

Le nœud tourne dans le conteneur `kynovara-testnet`. Une commande à la fois.

### D1. Créer la clé maîtresse (une seule fois)

```powershell
docker run --rm -it --entrypoint kynovara-cli kynovara:0.2.0 checkpoint master-new
```

24 nouveaux mots s'affichent : **c'est la clé maîtresse**, enregistrée nulle
part ailleurs. Écris-les sur papier (deux copies, deux lieux sûrs). La
commande affiche ensuite la **clé publique** : elle doit être inscrite dans
les paramètres du testnet (`checkpoint_master`), puis l'image reconstruite.
Pour la réafficher plus tard (les 24 mots sont demandés) :

```powershell
docker run --rm -it --entrypoint kynovara-cli kynovara:0.2.0 checkpoint master-pubkey
```

### D2. Autoriser le nœud à signer (tous les ~90 jours)

```powershell
docker exec -it kynovara-testnet kynovara-cli --datadir /data checkpoint delegate
```

Tape les 24 mots de la clé maîtresse. Le nœud signe ensuite chaque bloc.

### D3. Vérifier

```powershell
docker exec kynovara-testnet kynovara-cli --datadir /data checkpoint status
```

En cas d'**ALERTE** (signatures contradictoires : clé de signature volée ?),
la chaîne est gelée et les paiements suspendus. Révoque la clé affichée avec
`checkpoint revoke <clé>`, puis tranche avec `checkpoint sign` (24 mots
demandés à chaque fois).

---

## Sauvegardes

- **Phrase de 24 mots** : la vraie sauvegarde. Avec elle seule, la commande
  `wallet restore` recrée ton portefeuille sur n'importe quel PC.
- **`wallet.json`** + son mot de passe : pratique, mais pas suffisant seul (un
  disque peut mourir).
- La chaîne (`blocks\`) n'a pas besoin de sauvegarde : elle se retélécharge.

Pour réafficher les 24 mots (personne ne doit voir ton écran) :

```powershell
.\kynovara-cli.exe wallet show-mnemonic
```

---

## Dépannage

| Message | Solution |
|---|---|
| `le nœud ne répond pas sur 127.0.0.1:…` | Démarre `kynovarad` (fenêtre 1), avec le **même réseau**. |
| `jeton RPC introuvable` | Le nœud et `kynovara-cli` n'utilisent pas le même `--datadir`, ou le nœud n'est pas démarré. |
| `un autre nœud Kynovara utilise déjà le dossier` | Un nœud tourne déjà sur ces données : arrête-le ou utilise un autre `--datadir`. |
| `mot de passe incorrect (ou fichier de portefeuille modifié)` | Mauvais mot de passe, ou `wallet.json` altéré : restaure avec les 24 mots. |
| `horodatage … trop loin dans le futur` | L'horloge de ton PC est fausse : Paramètres → Heure et langue → « Synchroniser maintenant ». |
| `récompense de minage pas encore dépensable` | Attends que la récompense ait 100 blocs de confirmation. |
| `frais anormalement élevés` | Garde-fou : `--fee-rate` est sans doute trop grand (2 par défaut suffit). |
| `blkNNNNN.dat est corrompu à la position …` | Un fichier de la chaîne est abîmé (disque ?) ; le nœud refuse de démarrer plutôt que de détruire des blocs intacts. Supprime le dossier `blocks` pour retélécharger la chaîne depuis tes pairs. Si tu mines seul, les blocs de ta chaîne locale seront perdus (ton portefeuille et tes 24 mots, eux, ne sont pas touchés). |
| Le minage rend le PC lent | Réduis `--threads`, ou `mining stop`. |
