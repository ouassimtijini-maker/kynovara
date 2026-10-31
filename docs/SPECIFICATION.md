# Spécification technique de Kynovara (version 0.1)

Ce document décrit précisément les règles du protocole. Les règles marquées
**[consensus]** doivent être identiques sur tous les nœuds : les modifier après
le lancement d'un réseau provoque une scission (« fork ») de la chaîne. Le code
de référence est dans `crates/kynovara-core`.

## 1. Paramètres par réseau

| Paramètre | mainnet (non lancé) | testnet | regtest |
|---|---|---|---|
| Préfixe d'adresse (HRP) | `kyn` | `tkyn` | `rkyn` |
| Octets magiques (P2P) | `KYNM` | `KYNT` | `KYNR` |
| Ports P2P / RPC | 9733 / 9734 | 19733 / 19734 | 29733 / 29734 |
| Temps de bloc visé (T) | 120 s | 120 s | 120 s |
| Fenêtre LWMA (N) | 120 blocs | 60 blocs | — (difficulté fixe) |
| Cible minimale (`pow_limit`) | 2²³² − 1 | 2²⁴⁰ − 1 | 2²⁵⁵ − 1 |
| Cible de la genèse | à fixer au lancement | 2²²⁶ − 1 | 2²⁵⁵ − 1 |
| Horodatage max. dans le futur (FTL) | 720 s | 360 s | 86 400 s |
| Récompense initiale | 50 KYNO | 50 KYNO | 50 KYNO |
| Division par deux tous les | 1 051 200 blocs (≈ 4 ans) | 1 051 200 | 150 |
| Maturité des récompenses | 100 blocs | 100 | 100 |
| Type de pièce SLIP-44 | 9733 (provisoire) | 1 | 1 |
| Fin du rodage : premiers envois au bloc | 21 600 (proposition, ≈ 30 jours) | 6 000 | 0 (aucun rodage) |

Unités : 1 KYNO = 100 000 000 unités. Tous les montants sont des entiers
d'unités ; aucun nombre à virgule flottante n'intervient dans le consensus.
`MAX_MONEY` = 110 000 000 KYNO : aucun montant ni somme de montants ne peut
dépasser cette borne **[consensus]**.

## 2. Encodage binaire [consensus]

- Entiers à taille fixe, **petit-boutiste** (u8, u16, u32, u64).
- Exception : la **cible** dans l'en-tête de bloc est un entier de 256 bits en
  **gros-boutiste** (lisible : une cible difficile commence par des zéros).
- Listes : longueur sur u32, puis les éléments. Chaque liste a un maximum ;
  le décodeur vérifie aussi qu'il reste assez d'octets avant de réserver la
  mémoire.
- Un message doit être consommé **entièrement** : un octet en trop rend le
  message invalide. Il existe donc une seule représentation de chaque valeur.

## 3. Hachage [consensus]

- `sha256d(x) = SHA256(SHA256(x))` : empreinte des en-têtes de bloc.
- Hachage étiqueté (même construction que BIP-340) :
  `H_tag(x) = SHA256(SHA256(tag) ‖ SHA256(tag) ‖ x)`.

| Étiquette | Usage |
|---|---|
| `Kynovara/txid` | identifiant de transaction (sans signatures) |
| `Kynovara/wtxid` | identifiant avec signatures (arbre de Merkle) |
| `Kynovara/sighash` | message signé par les entrées |
| `Kynovara/merkle-leaf` | feuille de l'arbre de Merkle |
| `Kynovara/merkle-node` | nœud interne de l'arbre de Merkle |
| `Kynovara/pubkey-hash` | empreinte de clé publique (adresses) |

## 4. Clés, signatures, adresses

- Signatures **Ed25519** (RFC 8032). Une signature est valide **[consensus]**
  si et seulement si `VerifyingKey::verify_strict` de `ed25519-dalek` 2.2.0
  l'accepte et que la clé n'est pas d'ordre faible. La vérification stricte
  refuse les signatures non canoniques : un tiers ne peut pas modifier une
  signature existante. Les versions de `ed25519-dalek` et `curve25519-dalek`
  sont épinglées pour cette raison.
- Empreinte de clé : les 20 premiers octets de `H_pubkey-hash(clé publique)`.
- Adresse : Bech32m (BIP-350) du préfixe du réseau et des données
  `version (0) ‖ empreinte (20 octets)`. Exemple : `tkyn1q…`. Une adresse d'un
  autre réseau, d'une autre version ou de mauvaise longueur est refusée.

## 5. Transactions

### Format [consensus]

```
version      u32         (doit valoir 1)
type         u8          0 = récompense, 1 = transfert
— récompense :
  hauteur    u64         hauteur du bloc (rend chaque récompense unique)
  données    liste ≤ 64 octets (libres : message, nom de pool…)
— transfert :
  entrées    liste ≤ 1 000 de :
               txid précédent  32 octets
               index de sortie u32
               clé publique    32 octets
               signature       64 octets
sorties      liste ≤ 2 000 de :
               montant         u64
               destinataire    20 octets (empreinte de clé)
```

- `txid = H_txid(transaction encodée SANS les signatures)` : il ne dépend pas
  des signatures, donc les références entre transactions ne sont pas
  malléables.
- `wtxid = H_wtxid(transaction complète)`.
- Empreinte de signature (« sighash ») :
  `H_sighash(empreinte_genèse ‖ transaction_sans_signatures ‖ liste des sorties dépensées (montant, destinataire))`.
  Toutes les entrées signent ce même message. Il engage le réseau (pas de
  rejeu d'un réseau à l'autre) et les montants dépensés (les frais ne peuvent
  pas être falsifiés pour un appareil de signature).

### Règles sans contexte [consensus]

- version = 1 ; taille encodée ≤ 100 000 octets ;
- chaque sortie > 0 et ≤ MAX_MONEY ; somme ≤ MAX_MONEY ;
- transfert : au moins une entrée et au moins une sortie, aucune pièce
  dépensée deux fois dans la même transaction ;
- récompense : données ≤ 64 octets ; elle peut n'avoir **aucune sortie**
  (une fois l'émission terminée, un bloc sans frais ne rapporte rien, et une
  sortie de montant nul est interdite : sans cette exception, plus aucun bloc
  vide ne serait valide).

### Règles contextuelles [consensus]

Pour chaque entrée d'un transfert inclus à la hauteur `h` :

- la pièce référencée existe et n'est pas dépensée ;
- si c'est une récompense de minage créée à la hauteur `c` : `h ≥ c + 100` ;
- l'empreinte de la clé publique fournie est le destinataire de la pièce ;
- la signature est valide pour le sighash ;
- somme des entrées ≥ somme des sorties ; la différence est le **frais**.

## 6. Blocs

### En-tête (116 octets) [consensus]

| Champ | Octets | Encodage |
|---|---|---|
| version | 4 | u32 petit-boutiste (doit valoir 1) |
| bloc parent | 32 | empreinte |
| racine de Merkle | 32 | empreinte |
| horodatage | 8 | u64 petit-boutiste (secondes Unix) |
| cible | 32 | entier 256 bits gros-boutiste |
| nonce | 8 | u64 petit-boutiste |

L'empreinte du bloc est `sha256d(en-tête)`. Le bloc complet est l'en-tête
suivi de la liste des transactions (la première est la récompense).

### Arbre de Merkle [consensus]

Construit sur les `wtxid`. Feuilles : `H_merkle-leaf(wtxid)`. Nœuds :
`H_merkle-node(gauche ‖ droite)`. Un nœud sans partenaire est **remonté tel
quel** (pas de duplication comme dans Bitcoin) : deux listes de transactions
différentes ne peuvent pas avoir la même racine (faille CVE-2012-2459 évitée).

### Règles d'un bloc [consensus]

Sans contexte, dans cet ordre :

1. version = 1 ;
2. `0 < cible ≤ pow_limit` ;
3. **preuve de travail** : `sha256d(en-tête)`, lu comme entier gros-boutiste, ≤ cible ;
4. la racine de Merkle correspond aux transactions ;
5. au moins une transaction ; taille ≤ 1 000 000 octets ;
6. la première transaction est une récompense, aucune autre ne l'est ;
7. aucun txid en double ; chaque transaction respecte les règles sans contexte.

En contexte (parent connu, de hauteur `h − 1`) :

8. le parent n'est pas invalide ;
9. la cible est **exactement** celle calculée par LWMA (§ 7) ;
10. horodatage **strictement supérieur** à celui du parent ;
11. horodatage ≤ heure locale + FTL (720 s sur mainnet, 360 s sur testnet ;
    sinon refus **temporaire**, le bloc pourra être accepté plus tard). La
    limite vaut environ N·T/20, la recommandation de l'auteur de LWMA : un
    horodatage poussé à la limite ne rend le bloc suivant qu'environ 10 % plus
    facile ;
12. la récompense engage la hauteur `h` ;
13. **période de rodage** : si `h < transfers_unlock_height`, le bloc ne
    contient que sa récompense de minage (aucun transfert). Le bloc est refusé
    avant d'être stocké, et la règle est revérifiée à la connexion.

Validation complète (au moment de connecter le bloc à l'état UTXO) :

14. chaque transfert respecte les règles contextuelles (§ 5) ; un transfert
    peut dépenser une sortie créée plus tôt dans le même bloc ;
15. somme des sorties de la récompense ≤ subvention(h) + somme des frais ;
16. aucune sortie ne doit déjà exister non dépensée.

**Période de rodage** : pendant ce temps, on mine et les récompenses
s'accumulent, mais personne ne peut envoyer de pièces, pas même le fondateur.
La hauteur de fin est fixée dans le code : personne ne peut l'avancer. Les
nœuds refusent les envois en mempool (sans pénaliser le pair) et le mineur
n'en inclut aucun ; `info`, `wallet balance` et `wallet send` affichent le
bloc de fin et le temps restant.

Les sorties de la récompense du **bloc de genèse** ne sont jamais ajoutées à
l'ensemble UTXO (aucune pré-extraction).

## 7. Ajustement de difficulté : LWMA [consensus]

Pour le bloc de hauteur `h + 1` (parent à la hauteur `h`) :

- si `h < N` : cible = cible de la genèse ;
- sinon, avec les blocs `h − N … h` (N + 1 blocs) :

```
k = N × (N + 1) × T / 2
précédent = horodatage(h − N)
somme = 0 ; moyenne = 0
pour j = 1 … N (bloc i = h − N + j) :
    t = horodatage(i) si > précédent, sinon précédent + 1
    durée = min(t − précédent, 6 × T)
    précédent = t
    somme += durée × j
    moyenne += cible(i) / N / k       (divisions entières)
cible suivante = min(moyenne × somme, pow_limit), au moins 1
```

Les blocs récents pèsent plus lourd ; une longue pause est plafonnée à 6 T ;
un horodatage qui recule ne crée pas de durée négative. En regtest, la cible
vaut toujours `pow_limit`.

## 8. Travail et choix de la chaîne [consensus]

- Travail d'un bloc : `2²⁵⁶ / (cible + 1)`.
- La chaîne active est la chaîne **valide** de plus grand **travail cumulé**
  (pas la plus longue). À travail égal, la première reçue est conservée.
- Réorganisation : retour au point commun avec les données d'annulation (les
  1 000 derniers blocs en gardent en mémoire ; au-delà, l'état est
  reconstruit depuis la genèse, **à part**, et ne remplace l'état courant
  qu'en cas de réussite), puis connexion de la nouvelle branche. Si un bloc de
  la branche est invalide, il est marqué invalide avec tous ses descendants et
  le nœud revient à la meilleure chaîne valide. Une erreur de stockage pendant
  la bascule arrête le nœud (l'état est reconstruit depuis le disque au
  redémarrage) plutôt que de le laisser continuer sur un état douteux.
- Protection du disque (politique locale, hors consensus) : une branche
  concurrente qui part de moins de 1 000 blocs (en travail) sous le sommet est
  écrite sur le disque tout de suite, même plus légère (blocs concurrents
  ordinaires, réorganisations courantes : les produire coûte une vraie
  puissance de calcul). Une branche partant de **plus loin** et plus légère
  que la chaîne active n'est **pas écrite sur le disque** : elle est gardée
  dans une réserve en mémoire (100 000 blocs ou 64 Mio au plus), puis écrite
  (ancêtres d'abord) dès qu'elle revient à moins de 1 000 blocs du sommet.
  Quand la réserve est pleine, les feuilles invalides puis celles de plus
  faible travail sont oubliées : une longue branche bon marché partant de loin
  dans le passé ne peut ni remplir le disque ni évincer une vraie branche
  concurrente. Limites connues : une branche concurrente partant de plus de
  1 000 blocs sous le sommet et plus grosse que la réserve ne peut pas être
  reconstituée automatiquement ; une branche proche dont la difficulté a été
  abaissée en étirant ses horodatages peut faire écrire des blocs sur le
  disque pour moins cher que le minage honnête. La vraie solution, la
  synchronisation « en-têtes d'abord », est prévue avant le mainnet.

## 9. Émission [consensus]

```
subvention(h) = 5 000 000 000 unités >> (h / 1 051 200)   (0 si 64 divisions ou plus)
```

La subvention tombe à 0 à la 33ᵉ division (environ 132 ans après le
lancement) ; ensuite les mineurs ne sont payés que par les frais, et un bloc
sans frais a une récompense sans sortie (§ 5). Émission totale ≈ 105 120 000
KYNO (moins les 50 KYNO inutilisables de la genèse). Aucune pré-extraction.

## 10. Mempool (politique locale, hors consensus)

- ordre des vérifications : format, doublon, conflit, pièces dépensées,
  frais, place disponible — et **seulement ensuite** les signatures, qui sont
  vérifiées hors du verrou principal du nœud. Une transaction vouée au refus
  (sans frais, mempool pleine…) ne coûte presque rien à refuser ;
- frais minimaux : 1 unité par octet ;
- seules les pièces confirmées peuvent être dépensées (pas de chaînes de
  transactions non confirmées) ;
- pas de remplacement : la première transaction qui dépense une pièce gagne ;
- taille totale ≤ 64 Mio ; au-delà, les frais par octet les plus bas sont
  évincés ;
- après une réorganisation, les transactions des blocs retirés sont remises en
  mempool si elles sont encore valides (pour une réorganisation géante, celles
  des 1 000 blocs retirés les plus hauts seulement), puis toute la mempool est
  revérifiée contre le nouvel état ;
- un nouveau sommet fait reconstruire immédiatement le modèle de bloc du
  mineur ; de nouvelles transactions, au plus toutes les 2 secondes.

## 11. Réseau pair-à-pair

### Trame

`magique (4) ‖ commande (1) ‖ longueur u32 (≤ 1 004 096) ‖ contrôle (4 = sha256d(charge)[..4]) ‖ charge`.

### Messages

| Cmd | Message | Contenu |
|---|---|---|
| 0 | `version` | version du protocole, empreinte de la genèse, hauteur, nonce, port d'écoute, heure, logiciel (≤ 64 octets ASCII) |
| 1 | `verack` | — |
| 2 / 3 | `ping` / `pong` | nonce u64 |
| 4 | `getblocks` | localisateur (≤ 64 empreintes) + empreinte d'arrêt |
| 5 | `inv` | ≤ 2 000 × (type : 1 = tx, 2 = bloc ; empreinte) |
| 6 | `getdata` | idem |
| 7 | `notfound` | idem |
| 8 | `block` | bloc encodé |
| 9 | `tx` | transaction encodée |
| 10 / 11 | `getaddr` / `addr` | ≤ 1 000 × (IPv6 ou IPv4 mappée, port) |
| 12 | `mempool` | — (demande l'annonce des transactions en attente) |
| 13 | `checkpoint` | point de contrôle signé (§ 14) — protocole ≥ 2 |
| 14 | `revocation` | révocation d'une clé de signature (§ 14) — protocole ≥ 2 |

### Pairs à joindre

- `--connect IP:PORT` (répétable) : **uniquement** ces pairs, aucune
  découverte (utile pour un nœud abrité derrière un nœud public) ;
- sinon : les pairs `--addnode`, les **nœuds d'amorçage** du réseau
  (`seeds`, `params.rs` ; testnet : `51.222.143.109:19733`, désactivables avec
  `--noseed`) et, jusqu'à 8 connexions sortantes, des adresses du carnet.
  Les pairs configurés sont rejoints en permanence : 5 s après une
  déconnexion, puis avec un délai qui double à chaque échec (au plus 60 s).
  Un pair configuré qui se révèle être le nœud lui-même n'est plus rejoint.

### Déroulement

1. Présentation : chaque côté envoie `version` puis `verack`. Un pair d'un
   autre réseau (genèse différente) ou d'une version trop ancienne est
   déconnecté. Le nonce est tiré au hasard **pour chaque connexion** (nos
   connexions ne peuvent pas être reliées entre elles) ; recevoir l'un de nos
   propres nonces signale une connexion à soi-même (le côté entrant retrouve,
   par ce nonce, la connexion sortante et l'adresse qu'elle visait).
2. Synchronisation : `getblocks` → `inv` (500 blocs au plus) → `getdata` →
   `block`… jusqu'au sommet. Un bloc orphelin déclenche un nouveau
   `getblocks`. Un bloc déjà en cours de téléchargement auprès d'un autre pair
   n'est pas redemandé.
3. Une fois à jour par rapport à un pair, le nœud lui envoie `mempool` (une
   fois par connexion) ; le pair répond par l'annonce (`inv`) de ses 5 000
   transactions les mieux payées. Une transaction soumise avant l'arrivée d'un
   pair lui parvient donc.
4. Relais : un nouveau bloc ou une nouvelle transaction est annoncé par `inv`.
   Les transactions soumises localement (portefeuille, RPC) sont réannoncées
   toutes les 5 minutes tant qu'elles restent en attente.

### Protections

- taille des messages, nombre d'éléments et longueurs bornés à la lecture ;
- débit limité (jetons : 2 000, +500/s) : 1 jeton par message, 10 pour un
  bloc non demandé, 0 pour un bloc demandé (la synchronisation n'est jamais
  freinée), 1 + (nombre d'entrées) / 4 pour une transaction (autant de
  signatures à vérifier) ;
- une transaction refusée est mémorisée par son **wtxid** (signatures
  comprises) jusqu'au prochain bloc, et ignorée si elle est renvoyée : une
  copie altérée par un tiers ne peut pas faire ignorer la vraie (le txid,
  lui, ne couvre pas les signatures) ;
- délais : présentation 15 s, ping toutes les 60 s (réponse sous 90 s),
  inactivité 10 min, blocs demandés reçus sous 2 min ;
- score de mauvais comportement : un bloc ou une transaction invalide, un
  message illisible, une violation du protocole → déconnexion et bannissement
  pendant 24 h de l'adresse IPv4, ou du préfixe IPv6 /64 entier (sauf en
  boucle locale). Une erreur de stockage n'est jamais imputée au pair ;
- contre-pression : un objet demandé (bloc ou transaction) n'est envoyé que
  si moins de 32 Mio attendent déjà ; au-delà de 64 Mio en attente, quel que
  soit le message, le pair est déconnecté. Un même objet demandé plusieurs
  fois dans un `getdata` n'est envoyé qu'une fois. Les blocs sont lus sur le
  disque hors du verrou principal et envoyés tels quels ;
- au plus 32 connexions entrantes (dont 4 au plus par adresse IP ou préfixe
  IPv6 /64, hors boucle locale) et 8 sortantes par défaut, une seule sortante
  par groupe d'adresses (IPv4 /16, IPv6 /32) ;
- carnet d'adresses : une adresse annoncée par un pair n'est retenue que si
  elle est routable sur Internet (jamais de boucle locale ; adresses privées
  seulement entre deux machines d'un même réseau privé) ; une même source
  (IPv4, ou IPv6 /64) fournit au plus 1 000 adresses par 24 h, même en se
  reconnectant ; une adresse déjà jointe avec succès n'est pas évincée par
  des inconnues ;
- orphelins limités à 64 blocs et 16 Mo ; branches lointaines et plus
  légères gardées en mémoire seulement (§ 8).

## 12. Stockage

Fichiers `blocks/blk00000.dat`… (128 Mio chacun), enregistrements
`magique ‖ longueur ‖ SHA256(données)[..8] ‖ bloc`, synchronisés sur disque à
chaque écriture. Si une écriture échoue (disque plein, erreur matérielle), le
fichier est ramené à sa dernière position sûre et le stockage reste
utilisable. Au démarrage, les fichiers sont relus, l'index et l'état UTXO
sont reconstruits en revalidant chaque bloc. Un enregistrement abîmé en fin du
dernier fichier, sans aucun enregistrement intact après lui (coupure de
courant pendant une écriture), est tronqué automatiquement. Une corruption
suivie d'enregistrements intacts n'est **jamais** tronquée (ce serait détruire
des blocs encore bons) : le nœud refuse de démarrer et l'explique. Un verrou
de fichier empêche deux nœuds d'utiliser le même dossier.

## 13. Portefeuille

- Phrase de récupération **BIP-39** de 24 mots (liste anglaise officielle),
  graine par PBKDF2-HMAC-SHA512 (2 048 itérations), phrase de passe vide.
- Dérivation **SLIP-0010** Ed25519 : `m/44'/type'/0'/branche'/index'`
  (branche 0 = réception, 1 = monnaie rendue).
- Fichier `wallet.json` : Argon2id (64 Mio, 3 passes, 1 voie, sel de 16 octets)
  → clé de 32 octets ; XChaCha20-Poly1305 (nonce aléatoire de 24 octets)
  chiffre l'entropie de 32 octets ; les champs non secrets (réseau,
  paramètres, index) sont authentifiés comme données associées.
- Sélection des pièces : les plus grosses d'abord ; monnaie rendue vers une
  nouvelle adresse ; une monnaie inférieure à 10 000 unités part en frais.
- Garde-fou : une transaction dont les frais dépasseraient 1 KYNO n'est pas
  construite (presque toujours une erreur de saisie de `--fee-rate`).
- Limite d'écart : à chaque consultation du solde ou envoi, les 20 adresses
  suivant les index connus (réception et monnaie) sont aussi examinées ; des
  pièces trouvées là font avancer les index. Après une restauration, toute la
  fenêtre examinée (`--scan`, 100 par défaut) est considérée comme utilisée :
  une adresse vidée avant la restauration est indétectable et ne doit pas être
  redistribuée.

## 14. Points de contrôle signés (protection de lancement)

Règle de **choix de la chaîne** (pas de validité des blocs), active sur un
réseau dont les paramètres contiennent une clé maîtresse (`checkpoint_master`).
Code : `crates/kynovara-core/src/checkpoint.rs`, `Chain::apply_checkpoint`,
`crates/kynovara-node/src/checkpoints.rs`.

**Clés**
- **Clé maîtresse** : dérivée d'une phrase de 24 mots propre au fondateur
  (SLIP-0010, `m/44'/type'/9000'/0'/0'`), gardée hors ligne. Elle signe les
  délégations, les révocations et, en dernier recours, des points de contrôle.
- **Clé de signature** : autorisée par une délégation
  `{clé, hauteur_max, signature_maîtresse}` (environ 90 jours par défaut),
  installée sur **un seul** nœud, qui signe chaque nouveau sommet.

**Messages signés** (empreintes étiquetées, genèse comprise : un point d'un
réseau ne vaut rien sur un autre)
- point : `H("Kynovara/checkpoint", genèse ‖ hauteur u64 ‖ bloc)`, signé par la
  clé maîtresse (autorité 0) ou par une clé déléguée (autorité 1, délégation
  jointe) ;
- délégation : `H("Kynovara/delegation", genèse ‖ clé ‖ hauteur_max u64)` ;
- révocation : `H("Kynovara/revocation", genèse ‖ clé)`.

Encodage d'un point : `hauteur u64 ‖ bloc (32) ‖ autorité u8 [‖ clé (32) ‖
hauteur_max u64 ‖ signature (64)] ‖ signature (64)`.

**Validité d'un point** : signature exacte ; délégation signée par la clé
maîtresse, non expirée (`hauteur ≤ hauteur_max`) et non révoquée ;
`hauteur ≤ checkpoint_sunset_height` (au-delà, plus aucun point : le réseau
redevient purement fondé sur la preuve de travail).

**Effet sur la chaîne**
- toute branche qui ne contient pas le dernier point est écartée, quel que
  soit son travail ; un nouveau bloc dont la branche le contredit est refusé
  d'emblée (ni stocké ni indexé, sans pénalité pour le pair) ;
- un point sur une branche plus légère fait basculer la chaîne vers elle ;
- un point dont le bloc est inconnu reste en attente, et le bloc est demandé ;
- un point qui désigne un bloc invalide est annulé (retour au précédent).

**Contradiction** (deux points signés incompatibles : même hauteur, autre
branche, ou bloc invalide) : **alerte**. La chaîne est gelée (elle ne peut
plus que prolonger son sommet), les transactions sont refusées, plus aucune
signature n'est produite, et la contradiction est relayée à tout le réseau.
Seul un point de la **clé maîtresse** lève l'alerte (il s'impose même s'il
contredit le point courant) ; elle révoque en général la clé compromise en
même temps.

**Réseau** : à la fin de la présentation, un nœud envoie ses révocations, la
dernière décision de la clé maîtresse et son dernier point. Un point n'est
relayé que s'il vient d'être appliqué (ou s'il prouve une contradiction). Un
point déjà couvert par l'historique signé est ignoré sans vérifier ses
signatures ; une signature fausse vaut bannissement. Coût : 4 jetons de débit.

**Sur le disque** : `checkpoints.dat` (dernier point, dernière décision de la
clé maîtresse, révocations, alerte) et `signataire.json` (clé de signature,
fichier `0600`), dans le dossier du réseau.

**Points inscrits dans la version** (`release_checkpoints`) : à chaque
version du logiciel, le fondateur y ajoute le dernier point signé
(`kynovara-cli checkpoint export`). Un nœud qui démarre de zéro refuse alors
dès le départ tout bloc qui contredit ces points, avant même d'avoir reçu un
point signé : un attaquant qui l'isolerait ne peut pas lui faire accepter une
autre histoire au-delà. Dès que le bloc inscrit est connu, il devient le point
de contrôle de la chaîne (sauf point signé plus haut et cohérent). Tant que le
nœud n'a pas atteint ce point, `info` affiche « synchronisation incomplète ».

**Paramètres** : regtest — clé maîtresse publique de test (phrase « abandon »
× 23 puis « art »), sans fin ; testnet — clé du fondateur
`c74fc559…14ef6be`, fin à la hauteur 525 600 (environ 2 ans) ; mainnet — décidés au lancement.
