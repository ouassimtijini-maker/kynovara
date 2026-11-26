# Rapport d'audit — Kynovara 0.4.0

**Date** : 28 octobre 2026
**Périmètre** : tout le dépôt — `kynovara-core` (règles de consensus),
`kynovara-node` (réseau pair-à-pair, stockage, mineur, RPC),
`kynovara-wallet` (clés, portefeuille, `kynovara-cli`), dépendances et
documentation.
**Nature** : audit **interne** et volontairement sévère, en deux passes : une
relecture complète par l'auteur du code (Claude), puis une **relecture
indépendante** par une autre instance de Claude, lancée sans accès aux
raisonnements de l'auteur, qui a écrit et exécuté ses propres preuves de
concept, y compris sur les corrections de la première passe. Ce n'est **pas**
un audit externe professionnel : il en faudra un, par des humains spécialisés,
avant tout lancement avec de la vraie valeur (voir la fin du document).

**Nom** : le projet s'appelait « Keystone » pendant l'audit ; il a été renommé
« Kynovara » le 29 octobre 2026 (nom, symbole `KYNO`, préfixes `kyn` / `tkyn` /
`rkyn`, octets magiques et nouvelles genèses de testnet et de regtest). Aucune
autre règle n'a changé, et toute la suite de tests a été relancée après le
renommage.

## Verdict

| Usage | Verdict |
|---|---|
| Développement local, `regtest` | **Prêt.** |
| Réseau de test (`testnet`) entre tes machines ou avec des volontaires | **Prêt**, avec les limites connues ci-dessous. |
| Réseau principal (`mainnet`) avec de la vraie valeur | **Pas prêt.** Un point bloquant (R-02) ; R-01 est traité par RandomX en 0.4.0 (relu de façon indépendante, constats corrigés) mais doit encore être éprouvé sur le testnet (activation au bloc 21 000) ; restent les décisions de `docs/DECISIONS.md`, un audit externe et la phase juridique de `docs/LANCEMENT.md`. Le nœud refuse d'ailleurs de démarrer sur le mainnet tant que sa genèse n'existe pas. |

Aucun constat ouvert ne permet aujourd'hui de **créer de la monnaie**, de
**dépenser les pièces d'autrui** ou de faire accepter **deux dépenses de la
même pièce** dans une chaîne. La double dépense par réécriture de
l'historique (attaque des 51 %) reste possible pour qui réunit la majorité de
la puissance de calcul (R-01) : depuis la 0.4.0, c'est de la puissance
**RandomX** (des processeurs), et non plus la puissance SHA-256 des machines
spécialisées de Bitcoin, louable à l'heure pour presque rien.

Bilan : **41 constats corrigés**, dont 7 de gravité élevée (4 trouvés par
l'auteur, 3 par le relecteur indépendant), chacun avec un test de
non-régression quand c'est possible ; le relecteur a aussi jugé insuffisante
une correction de la première passe (carnet d'adresses, M-4), reprise depuis.
**17 limites connues** sont documentées ci-dessous. La version 0.2.0 ajoute
les **points de contrôle signés** (section dédiée ci-dessous) : 6 problèmes
trouvés en les auditant, tous corrigés avant livraison. La version 0.4.0
remplace la preuve de travail SHA-256 par **RandomX** (section dédiée
ci-dessous) : 3 constats de la relecture de l'auteur et 8 de la relecture
indépendante, dont un de gravité élevée (V-1), tous corrigés sauf V-7
(documenté, R-17).

---

## Méthode

1. **Relecture ligne à ligne** de tout le code, en commençant par les règles
   de consensus (validation, choix de chaîne, réorganisations, difficulté,
   arithmétique 256 bits, encodage), puis le réseau, le stockage, le RPC et
   le portefeuille. Pour chaque fonction : que se passe-t-il avec une entrée
   hostile, au bord des limites, après une panne ?
2. **Relecture indépendante** avec preuves de concept exécutées contre de
   vrais nœuds (faux pairs sur de vraies sockets, mesure de la mémoire et des
   temps de réponse), un **test différentiel** de la chaîne (10 graines × 140
   blocs sur des branches aléatoires, blocs invalides selon le contexte,
   données d'annulation et réserves minuscules, comparaison après chaque bloc
   avec un rejeu indépendant) et une injection de pannes de stockage. Toutes
   ses preuves de concept font désormais partie de la suite de tests.
3. **Analyse statique** : `cargo clippy` en mode strict (tout avertissement
   est une erreur), code `unsafe` **interdit** dans tout le projet, `unwrap`
   interdit hors des tests, débordements arithmétiques contrôlés même en
   version optimisée (`overflow-checks = true`) ; le relecteur a vérifié
   qu'aucun calcul contrôlé par un pair ou par le RPC ne peut déborder.
4. **Tests** (223 depuis la 0.4.0, tous verts en mode optimisé sous Linux) :
   - vecteurs officiels : RFC 8032 (Ed25519), BIP-39 (8 vecteurs), SLIP-0010,
     BIP-350 (valides et invalides), FIPS 180 (SHA-256), RandomX (4 vecteurs
     du dépôt de référence tevador/RandomX) ;
   - règles de consensus une par une, réorganisations (y compris au-delà des
     données d'annulation), branches invalides, orphelins, fin de l'émission,
     test aléatoire et test différentiel ;
   - « fuzzing » déterministe : 300 000 entrées aléatoires ou altérées ne font
     jamais paniquer les décodeurs, et tout ce qui se décode se ré-encode à
     l'identique (encodage canonique) ;
   - tests adverses avec de **faux pairs malveillants** : mauvais réseau,
     message avant la présentation, trame géante, bloc altéré, demandes
     massives, connexion à soi-même, branche concurrente plus légère,
     inondation de transactions sans frais, censure par copie altérée,
     empoisonnement du carnet d'adresses, adresses internes ;
   - pannes simulées : écriture partielle, synchronisation ratée, lecture
     impossible pendant une reconstruction de l'état ;
   - plusieurs **vrais nœuds** qui se synchronisent, se réorganisent à la
     reconnexion, redémarrent depuis le disque, se transmettent les
     transactions en attente ;
   - scénario complet avec le **vrai binaire** `kynovara-cli` (création,
     minage, maturité, envoi, restauration par les 24 mots, limite d'écart).
5. **Essais réels** : minage sur le testnet à la vraie difficulté (environ
   10 MH/s avec 2 fils), arrêt brutal du nœud pendant l'écriture puis
   redémarrage.
6. **Chaîne d'approvisionnement** : chaque dépendance comparée à la base
   d'avis de sécurité RustSec (état du 27 octobre 2026) : **aucun avis**
   ne touche les versions utilisées ; toutes les versions exigées existent sur
   crates.io et aucune n'est retirée (index du 28 octobre 2026). Les
   bibliothèques de signature (`ed25519-dalek` 2.2.0, `curve25519-dalek`
   4.1.3) sont **épinglées** : la vérification d'une signature est une règle
   de consensus et ne doit jamais changer par une mise à jour automatique
   (la 4.2.0 de `curve25519-dalek` a d'ailleurs été retirée par ses auteurs).
   L'intégration continue fournie relance `cargo audit` chaque semaine.

Échelle de gravité : **Critique** (perte de fonds ou scission du réseau,
exploitable), **Élevée** (déni de service grave, perte de données, ou piège
latent menant à une perte de fonds), **Moyenne**, **Faible**, **Info**.

---

## Version 0.4.0 : preuve de travail RandomX

**Changement de règle de consensus** (bifurcation programmée du testnet).

- **Testnet** : SHA-256 double jusqu'au bloc 20 999, **RandomX à partir du
  bloc 21 000** (vers le 2 décembre 2026). L'ajustement de difficulté repart
  de zéro : cible 2²⁴² − 1 pour les blocs 21 000 à 21 060, puis LWMA sur des
  blocs RandomX uniquement. Un nœud 0.3.x refuse les blocs RandomX et reste
  sur l'ancienne chaîne : **tous les nœuds doivent passer en 0.4.0 avant le
  bloc 21 000**, à commencer par le serveur d'amorçage et le nœud signataire.
  Activation prévue d'abord au bloc 16 000 : ce bloc est passé vers le
  25 novembre alors qu'aucun nœud n'était en 0.4.0, donc sans scission (la
  chaîne a continué en SHA-256) ; repoussée le 26 novembre, hauteur alors
  16 838.
- **Mainnet** : RandomX dès la genèse (cibles provisoires, à fixer au
  lancement). **Regtest** : SHA-256 double (blocs instantanés pour les tests).
- **Clé RandomX** : calendrier de Monero (changement tous les 2 048 blocs,
  64 blocs après le bloc qui la fournit), prise **sur la branche du bloc**.
- **Mise en œuvre** : `randomx-rs` 1.6.0 (projet Tari), liaison vers
  l'implémentation C++ de référence tevador/RandomX, épinglée à la version
  exacte (règle de consensus) ; **mode léger** (cache de 256 Mio par clé,
  ≈ 15 ms par empreinte, ≈ 60 essais/s par fil, mesurés sous Docker) ; un
  fil dédié vérifie les blocs et garde les caches des deux dernières clés ;
  chaque fil de minage a son propre cache, gardé d'un bloc à l'autre (le
  mineur a désormais des fils permanents).
- Au démarrage, les blocs relus depuis le disque du nœud ne sont pas
  revérifiés en RandomX (ils l'ont été avant d'être écrits) ; toutes les
  autres règles le sont.

**Nature de la vérification**, le 21 novembre 2026, en trois temps, comme
pour les versions précédentes :
1. relecture par l'auteur du code (Claude), avec ses tests ;
2. **relecture structurée** par la même instance (six angles : lecture ligne
   à ligne, comportements retirés, appelants, pièges du langage, enveloppes,
   conventions) : 6 constats, dont 3 corrigés (RX-1 à RX-3) ;
3. **relecture indépendante** par une autre instance de Claude, lancée sans
   accès aux raisonnements de l'auteur, qui a écrit et exécuté ses propres
   preuves de concept (dans Docker, et sous un profil seccomp pour V-2) :
   8 constats (V-1 à V-8), dont un de gravité élevée que les deux premières
   passes avaient manqué. Tous sont corrigés sauf V-7 (documenté, R-17), et
   ses preuves de concept font désormais partie de la suite de tests
   (`tests/relecture_*.rs`), retournées en tests de non-régression.

33 nouveaux tests en tout (223). Essai réel : la 0.4.0 a relu une copie de la
chaîne du testnet (13 031 blocs, sans réseau) avec les nouvelles règles, sans
refuser aucun bloc (version antérieure à la correction V-1, sans effet sur ces
blocs). Ces blocs étant tous antérieurs à l'activation, cet essai
vérifie la compatibilité avec la chaîne existante, **pas** le code RandomX,
qui ne sera éprouvé en réel qu'à partir du bloc 21 000. Compilée et testée sous Linux
(Docker), puis par l'intégration continue sous Linux et Windows ; sur le PC du
fondateur, Smart App Control a bloqué le
script de compilation de `randomx-rs` (voir R-08). Chaîne
d'approvisionnement : `cargo audit` du 21 novembre 2026 (1 555 avis RustSec,
68 dépendances, dont `randomx-rs` 1.6.0 et ses dépendances `bitflags`,
`libc`, `thiserror` et `cmake`) : **aucun avis**.

Points examinés :

| Question | Réponse | Test |
|---|---|---|
| Un bloc peut-il échapper à la preuve en mentant sur sa hauteur ? | Non : sans contexte, l'algorithme est choisi d'après la hauteur annoncée par la récompense ; en contexte, cette hauteur doit être la vraie, et la preuve RandomX est calculée d'après la vraie hauteur. | `structure_check_uses_the_claimed_height` |
| Une preuve SHA-256 suffit-elle encore après l'activation ? | Non. | `blocks_switch_to_randomx_at_the_activation_height` |
| Une preuve calculée avec la clé d'une autre branche est-elle acceptée ? | Non : la clé est l'ancêtre du bloc sur sa propre branche ; une branche concurrente qui la dépasse est adoptée normalement. | `randomx_key_rotates_and_belongs_to_the_branch` |
| Une erreur interne de RandomX (mémoire) peut-elle faire rejeter un bloc valide ou bannir un pair honnête ? | Non : erreur `PowUnavailable`, ni définitive ni imputée au pair ; le bloc n'est pas indexé et pourra être revu. | revue |
| La relecture du disque sans RandomX peut-elle faire accepter un bloc non vérifié ? | Seuls nos propres fichiers sont relus ainsi. Un nœud 0.3.x qui aurait continué après l'activation aurait écrit des blocs SHA-256 dont la cible ne correspond pas aux nouvelles règles : refusés à la relecture. | `stored_blocks_skip_only_the_randomx_check` |
| Un orphelin RandomX est-il vérifié ? | Oui, quand son parent arrive (avant, sa clé n'est pas connue). | `randomx_orphans_are_checked_once_their_parent_arrives` |
| La bibliothèque calcule-t-elle le vrai RandomX ? | Oui : 4 vecteurs officiels. | `official_test_vectors` |
| Du code `unsafe` a-t-il été ajouté ? | Non : les objets de `randomx-rs` ne peuvent pas changer de fil ; chaque fil possède les siens. | compilation (`unsafe_code = "forbid"`) |

Constats de la relecture structurée :

| ID | Gravité | Constat | Correction | Test |
|---|---|---|---|---|
| RX-1 | Moyenne | Un orphelin RandomX (parent inconnu) entrait dans la réserve sans aucune preuve ; un bloc d'avant l'activation pouvait même annoncer une hauteur RandomX pour échapper à la preuve SHA-256. N'importe qui remplissait la réserve gratuitement et en chassait les vrais orphelins. | Un orphelin RandomX n'est gardé que si la clé de la hauteur qu'il annonce est un bloc de la chaîne active (bloc proche du sommet) ; sinon il est ignoré, sans calcul ni pénalité, et sera redemandé pendant la synchronisation. Un orphelin proche reste gratuit à fabriquer, comme l'était presque un orphelin SHA-256 avant. | `randomx_orphans_far_ahead_are_not_kept` |
| RX-2 | Moyenne | RandomX indisponible (mémoire) : les refus n'étaient journalisés qu'au niveau « debug », le nœud cessait de suivre la chaîne sans rien dire. | Journalisés en erreur. | revue |
| RX-3 | Moyenne | Nombre de fils de minage par défaut = cœurs − 1, soit 256 Mio chacun en RandomX : sur une machine à beaucoup de cœurs et peu de mémoire, mémoire épuisée à l'activation. Idem pour l'outil de genèse (tous les cœurs). | Au plus 4 fils par défaut (nœud et genèse) ; la mémoire prévue est annoncée au démarrage du minage. | revue |

Non corrigés : le calcul RandomX sous le verrou principal (R-14), le pointeur
de machine virtuelle non vérifié par `randomx-rs` (R-13), et l'absence de test
de bout en bout du mineur du nœud en RandomX (ses fils de calcul sont testés
seuls).

Constats de la relecture indépendante :

| ID | Gravité | Constat | Correction | Test |
|---|---|---|---|---|
| V-1 | **Élevée** | **Le travail SHA-256 écrasait le travail RandomX.** Les deux travaux s'additionnaient : un bloc SHA-256 du testnet (≈ 2³⁰ essais) pesait autant que ≈ 65 000 blocs RandomX (≈ 2¹⁴). Une branche partant d'avant l'activation, avec quelques blocs SHA-256 datés à une seconde d'intervalle (LWMA relève alors leur difficulté), dépassait toute la chaîne RandomX honnête : en 2 blocs, ≈ 2 200 blocs RandomX de surplus ; en 10, ≈ 120 000 (5,5 mois), pour quelques secondes de carte graphique. Seul un point de contrôle signé sur un bloc RandomX l'empêchait : signataire éteint au moment de l'activation, délégation expirée ou alerte suffisaient à ouvrir la fenêtre, puis double dépense. | Travail cumulé à deux composantes (`ChainWork`) : les branches se comparent d'abord au travail RandomX, puis au travail SHA-256 (spécification, § 8). Identique aux règles de la 0.3.x avant l'activation (aucun travail RandomX de part et d'autre). | `relecture_deux_blocs_sha_d_avant_l_activation_n_effacent_plus_les_blocs_randomx`, `relecture_seul_un_point_de_controle_randomx_empeche_cette_reecriture`, `relecture_echelle_du_testnet` |
| V-2 | Moyenne | **Plantage quand le système refuse la mémoire exécutable** (systemd `MemoryDenyWriteExecute=yes`, SELinux `deny_execmem`, PaX) : la version de RandomX livrée avec `randomx-rs` ignore l'échec de `mprotect`, la création du cache réussit quand même, et le repli sur l'interpréteur ne se déclenchait jamais : SIGSEGV au premier calcul, puis boucle de redémarrages. L'affirmation de R-13 (« le cache signale proprement ce refus ») était fausse. | `kynovarad` essaie le JIT au démarrage dans un processus fils (`pow::self_test`) et passe à l'interpréteur s'il échoue ; `KYNOVARA_RANDOMX_JIT=0` le désactive d'office. Vérifié sous le profil seccomp du relecteur : plantage reproduit (code 139) sans la correction, démarrage normal et repli sur l'interpréteur avec. | `interpreter_and_jit_give_the_same_hashes`, `tests/relecture_jit.rs`, essai seccomp |
| V-3 | Moyenne | **64 vérifications RandomX imposées par bloc** : un pair envoyait 64 enfants faux d'un bloc P avant P (non sollicités : sous le plafond de débit) ; à l'arrivée de P, 65 empreintes sous le verrou principal (1,09 s mesurée au lieu de 18 ms), sans bannissement ; répétable à chaque bloc, et pendant la synchronisation (≈ ×60). R-14 (« une empreinte par connexion ») était donc optimiste. | Au plus 2 orphelins par parent manquant (`MAX_ORPHANS_PER_PARENT`) ; un orphelin écarté est redemandé pendant la synchronisation. | `relecture_les_orphelins_randomx_faux_sont_limites_par_parent` |
| V-4 | Faible | Bannissement de 24 h des nœuds restés en 0.3.x : leurs blocs SHA-256 après l'activation (cible fausse) faisaient bannir leur adresse par le serveur d'amorçage, qui leur restait fermé une fois mis à jour. | Une cible fausse (vérification bon marché) déconnecte sans bannir. | revue |
| V-5 | Info | Un orphelin rattaché dont la preuve RandomX n'a pas pu être calculée était jeté sans trace (RX-2 incomplet). | Signalé dans le compte rendu du bloc et journalisé en erreur. | revue |
| V-6 | Info | Mineur : si un fil de calcul mourait après avoir reçu un modèle, le fil « mineur » pouvait l'attendre indéfiniment. Aucune panique atteignable n'a été trouvée. | Un canal de fin par modèle ; un fil mort n'est plus attendu. | `a_late_completion_signal_cannot_count_for_the_next_template` |
| V-7 | Info | La relecture « de confiance » du disque suppose des règles inchangées entre l'écriture et la relecture : un changement futur des paramètres RandomX, ou un dossier `blocks` venu d'ailleurs, ferait relire des blocs RandomX jamais vérifiés. | Non corrigé : voir R-17. | — |
| V-8 | Info | Tests qui promettaient plus qu'ils ne vérifiaient (partage des nonces, réutilisation du calculateur), chemin « RandomX dès la genèse » jamais exécuté, essai réel sans aucun bloc RandomX. | Tests renommés selon ce qu'ils vérifient ; test du démarrage d'un réseau en RandomX dès la genèse ; portée de l'essai réel précisée ci-dessus. | `a_network_can_start_with_randomx_from_genesis` |

Affirmations de l'auteur confirmées par le relecteur : vecteurs officiels,
refus d'une preuve SHA-256 après l'activation, inutilité d'un mensonge sur la
hauteur, clé prise sur la branche du bloc, refus à la relecture des blocs
écrits par une 0.3.x après l'activation, aucun code `unsafe`, mesures
(≈ 16,5 ms par empreinte, ≈ 0,35 s par cache). Zones jugées saines :
calendrier des clés (identique au `rx_seedheight` de Monero), choix de
l'algorithme d'après la hauteur réelle, LWMA et ses bornes par algorithme,
mineur (nonces disjoints, arrêt, changement de clé, libération de la
mémoire), objets RandomX jamais partagés entre fils.

---

## Version 0.3.2 : réseau de test public

Aucun changement de règle de consensus. Le testnet inscrit un nœud
d'amorçage (`51.222.143.109:19733`), joint automatiquement sauf avec
`--connect` ou `--noseed`, et un nouveau point de contrôle de version
(bloc 1 398).

Constat corrigé au passage (**Faible**) : une connexion à soi-même n'était
détectée que du côté entrant (la connexion sortante parle la première), qui
ignore l'adresse visée ; un pair configuré pointant vers le nœud lui-même
(cas du serveur d'amorçage) était donc retenté toutes les 5 secondes, et une
telle adresse du carnet n'était jamais retirée. Le côté entrant retrouve
désormais la connexion sortante par son nonce ; l'adresse est retirée du
carnet et n'est plus jamais rejointe (tests `tests/seeds.rs`).

---

## Version 0.3.1 : avis et licence

Aucun changement de règle : ajout de `AVIS.md` (réseau de test, jetons sans
valeur, aucune vente, aucune garantie), affiché aussi par `kynovarad`,
`kynovara-cli info`, `wallet balance` et l'aide des deux programmes sur les
réseaux de test ; licences MIT et Apache-2.0.

---

## Version 0.3.0 : période de rodage

Règle de consensus (`docs/SPECIFICATION.md`, § 6, règle 13) : avant
`transfers_unlock_height`, un bloc ne contient que sa récompense. Vérifiée à
quatre endroits : avant le stockage d'un bloc, à sa connexion (y compris lors
d'un rejeu complet), à l'entrée en mempool et dans le modèle du mineur. Un
retour sous la hauteur de fin (réorganisation) vide la mempool. Un pair qui
envoie un transfert pendant le rodage n'est pas pénalisé ; un bloc qui en
contient, si.

**Vérifié par les tests** (4 nouveaux) : refus en mempool, bloc refusé et non
stocké, rejeu complet, minage normal pendant le rodage, même transfert accepté
partout au bloc de fin, modèle du mineur et mempool après un retour en
arrière, messages du portefeuille (`no_transfer_before_the_unlock_height_then_normal`,
`going_back_into_the_lock_drops_pending_transfers`,
`transfer_lock_notice_counts_down`, `transfer_lock_ends_before_checkpoints_do`).
Le testnet existant (aucun envoi jusqu'ici) reste valide sous la nouvelle règle.

**Limites** : la date de fin ne peut être ni avancée ni repoussée sans une
nouvelle version adoptée par tous les nœuds ; rien n'empêche de vendre ses
24 mots hors du réseau.

---

## Version 0.2.0 : points de contrôle signés

Ajout de la protection décrite dans `docs/SPECIFICATION.md`, § 14 : clé
maîtresse hors ligne (24 mots), clé de signature déléguée qui signe chaque
bloc, refus de toute branche contredisant le dernier point, alerte et gel en
cas de contradiction, révocation, fin programmée.

**Vérifié par les tests** (20 nouveaux) : un attaquant avec deux fois plus de
travail ne fait pas basculer un nœud honnête, et son propre nœud est ramené
sur la chaîne signée dès qu'il reçoit le point (`majority_attacker_cannot_rewrite_signed_history`) ;
une signature contradictoire gèle la chaîne, suspend les transactions et
ignore les signatures déléguées jusqu'à la décision de la clé maîtresse, la
clé révoquée ne signe plus rien (`stolen_key_contradiction_freezes_until_master_decides`) ;
points, alerte et clé de signature survivent à un redémarrage ; délégations
fausses, échues ou ne correspondant pas à leur clé refusées ; points d'un
autre réseau, falsifiés ou rejoués refusés ; un point sur un bloc invalide est
annulé ; parcours complet du fondateur avec le vrai `kynovara-cli`
(`founder_checkpoint_commands`).

**Problèmes trouvés en auditant la fonctionnalité (tous corrigés)** :

| ID | Gravité | Constat | Correction |
|---|---|---|---|
| K-1 | Élevée | **Blocage du nœud** : une condition prenait deux fois le même verrou dans une seule expression (`a() \|\| b()`), d'où un interblocage dès qu'un point inconnu arrivait. | Verrou pris une seule fois. |
| K-2 | Moyenne | **Saturation du processeur** : chaque point rediffusé (même ancien et authentique) faisait vérifier deux signatures ; 32 pairs pouvaient occuper un cœur. | Un point déjà couvert par l'historique signé est ignoré sans vérification ; 4 jetons de débit par point ; signature fausse = bannissement. |
| K-3 | Moyenne | Un nœud absent pendant une alerte recevait les points postérieurs à la décision de la clé maîtresse sans cette décision : alerte injustifiée et permanente. | La dernière décision de la clé maîtresse est conservée et envoyée à chaque pair avant le dernier point. |
| K-4 | Moyenne | Jusqu'à 10 000 révocations envoyées à chaque nouveau pair : dépassement de sa limite de débit, donc bannissement mutuel. | 256 révocations au plus ; une révocation déjà connue est ignorée sans vérification. |
| K-5 | Faible | Une contradiction sous le dernier point, sur un bloc inconnu, restait « en attente » au lieu de déclencher l'alerte. | Sous le dernier point, tout autre bloc est une contradiction, connu ou non. |
| K-6 | Faible | La clé secrète de signature pouvait partir en clair vers un nœud distant (`--rpc`). | `checkpoint delegate` refuse tout nœud non local. |

**Limites de cette protection** :
- elle **centralise** : pendant la phase de lancement, l'historique est celui
  que signe le fondateur. Clé maîtresse volée = contrôle de la chaîne ; clé
  maîtresse perdue = plus de révocation ni de levée d'alerte. D'où la fin
  programmée, à annoncer publiquement ;
- **une seule machine** doit signer : deux nœuds signataires peuvent signer
  deux sommets différents et déclencher une alerte ;
- un nœud qui démarre de zéro suit la chaîne la plus lourde tant qu'il n'a pas
  reçu de point de contrôle. **Atténué en 0.2.1** : chaque version inscrit le
  dernier point signé (`release_checkpoints`) ; un attaquant qui isole un
  nouveau nœud ne peut plus lui faire accepter d'histoire contredisant ce
  point (test `release_checkpoints_protect_a_fresh_node_from_the_start`). Il
  peut seulement l'empêcher d'avancer, ce que `info` signale ;
- un point signé par une clé volée sur un bloc inconnu, au-delà du dernier
  point, reste en attente sans alerte (sans effet tant que le bloc n'existe
  pas sur une branche valide) ;
- les anciens nœuds (protocole 1) ne reçoivent pas les points : ils ne sont
  pas protégés.

---

## Constats de la relecture indépendante (tous corrigés)

| ID | Gravité | Constat | Correction | Test de non-régression |
|---|---|---|---|---|
| H-1 | Élevée | **Saturation du processeur et du verrou principal par des transactions sans frais** : toutes les signatures étaient vérifiées *avant* le contrôle des frais, sous le verrou principal. Une transaction de 740 entrées correctement signées mais sans frais coûtait 45 à 60 ms ; renvoyée en boucle par 4 connexions, elle bloquait le nœud 14 à 17 s, sans aucune sanction. | Frais et place disponible vérifiés **avant** les signatures ; signatures vérifiées **hors du verrou** (puis revérification sous le verrou avant insertion) ; une transaction déjà refusée et renvoyée est ignorée sans rien vérifier ; une transaction coûte au pair 1 + (entrées / 4) jetons de débit. | `zero_fee_flood_is_cheap_to_refuse_and_cut_off`, `signatures_are_checked_last` |
| H-2 | Élevée | **Épuisement de la mémoire** : les réponses `tx` à un `getdata` n'avaient aucune limite en octets (la contre-pression ne visait que les blocs). Deux messages demandant 2 000 fois la même transaction de 56 Ko faisaient grossir le nœud de 221 Mo ; jusqu'à ~400 Mo par connexion. | Limite absolue de 64 Mio en attente par pair pour **tout** message (au-delà : déconnexion) ; contre-pression pour les transactions comme pour les blocs ; un objet demandé plusieurs fois n'est envoyé qu'une fois. | `getdata_flood_does_not_grow_memory`, `getdata_for_unknown_items_gets_notfound` |
| H-3 | Élevée | **Censure ciblée d'une transaction** : les refus étaient mémorisés par txid, qui ne couvre pas les signatures. Envoyer une copie de T à la signature altérée faisait ignorer pour toujours la vraie T. Les refus « frais trop faibles » restaient aussi mémorisés indéfiniment. | Refus mémorisés par **wtxid** (signatures comprises), vidés à chaque nouveau bloc ; plus aucun filtrage des annonces sur ce critère. | `malleated_copy_cannot_censor_the_real_transaction` |
| M-1 | Moyenne | Une transaction n'était annoncée qu'**une fois** : soumise sans pair connecté (ou annonce perdue), elle ne partait jamais, alors que le portefeuille affichait « Transaction diffusée » et bloquait les pièces. | Nouveau message `mempool` : une fois à jour, un nœud demande à chaque pair ses transactions en attente ; les transactions locales sont réannoncées toutes les 5 minutes jusqu'à confirmation. | `pending_transaction_reaches_peers_that_connect_later` |
| M-2 | Moyenne | **Une écriture ratée rendait le stockage inutilisable** (disque plein, erreur matérielle) : les blocs suivants étaient enregistrés à une mauvaise position, illisibles, et le nœud refusait de redémarrer. Par ricochet, il bannissait un pair honnête à chaque nouveau bloc. | Après toute erreur d'écriture, le fichier est ramené à sa dernière position sûre (sinon le stockage refuse d'écrire jusqu'au redémarrage) ; une erreur de stockage n'est plus jamais imputée à un pair. | `failed_write_leaves_store_usable` |
| M-3 | Moyenne | Mémoire croissante avec l'historique : les données d'annulation des 10 000 derniers blocs restaient en mémoire (5 à 6,6 Go avec des blocs pleins) ; l'index des transactions confirmées grandit indéfiniment. | Données d'annulation limitées aux 1 000 derniers blocs (environ 660 Mo au pire). L'index des transactions reste une limite connue (R-03). | revue |
| M-4 | Moyenne | **Carnet d'adresses** : la limite de 1 000 adresses par pair se remettait à zéro à chaque reconnexion (carnet entier rempli depuis une seule IP) ; des adresses de **boucle locale et privées** fournies par un pair étaient contactées (le nœud pouvait être dirigé vers un service interne) ; aucune limite pour IPv6 (bannissement par adresse exacte, contourné en changeant d'adresse dans son /64). | Quota par source sur 24 h, toutes connexions confondues ; seules les adresses routables sont retenues (adresses privées uniquement entre machines d'un même réseau privé, jamais de boucle locale) ; bannissement et limite d'entrées par /64 IPv6, diversité des sortants par /32 IPv6 ; les adresses éprouvées ne sont pas évincées. | `address_book_cannot_be_flooded_or_aimed_at_internal_services`, `network_supplied_addresses_are_filtered_and_rate_limited`, `proven_addresses_survive_a_flood`, `routability` |
| L-1 | Faible | La reconstruction de l'état (réorganisation plus profonde que les données d'annulation) n'était pas atomique : une erreur de lecture laissait un état à moitié reconstruit, où une pièce dépensée réapparaissait et où la mempool acceptait sa double dépense. | Reconstruction à part, remplacement seulement en cas de réussite ; une erreur de stockage pendant une bascule **arrête le nœud** (état reconstruit depuis le disque au redémarrage). | `failed_rebuild_keeps_state_consistent_and_stops_the_chain` |
| L-2 | Faible | Les blocs demandés par les pairs étaient lus sur le disque et décodés **sous le verrou principal**, et chaque lecture lisait le bloc deux fois. | Seul l'emplacement est pris sous le verrou ; lecture unique, hors verrou, et envoi des octets tels quels. | `location_reads_without_the_store`, `raw_block_frame_matches_encoded_message` |
| L-3 | Faible | Windows : la documentation proposait des dossiers à la racine de `C:\`, modifiables par défaut par tous les comptes de la machine (jeton RPC, portefeuille et programmes exposés aux autres comptes). | Projet dans le dossier personnel, données dans `%APPDATA%` (protégé par le profil) ; mise en garde contre `--datadir` à la racine (voir aussi R-11). | revue |
| L-4 | Faible | Sous Windows, Git convertit les fins de ligne : la vérification de l'empreinte officielle de la liste BIP-39 aurait échoué. | Fichier `.gitattributes` ; vérification insensible aux fins de ligne. | `wordlist_is_the_official_one` |
| L-5 | Faible | Après une restauration, les index d'adresses étaient sous-estimés (seules les adresses portant encore des pièces étaient comptées) : adresses déjà données redistribuées, paiements ultérieurs invisibles. | Toute la fenêtre examinée compte comme utilisée ; « limite d'écart » de 20 adresses à chaque consultation : des pièces trouvées au-delà font avancer les index. | `full_wallet_scenario` (adresse n° 4 découverte) |
| I-1 | Info | Réserve en mémoire (P-01) : une branche concurrente plus lourde dont la partie encore plus légère dépasse ~64 blocs pleins ne pouvait pas être adoptée (scission). | Les branches partant de moins de 1 000 blocs sous le sommet sont écrites sur le disque d'emblée ; seules les plus lointaines passent par la réserve (voir R-02). | `recent_forks_go_to_disk_deep_lighter_forks_stay_in_memory` |
| I-2 | Info | Le même nonce `version` sur toutes les connexions permettait de relier les connexions d'un nœud entre elles. | Nonce tiré pour chaque connexion ; détection de la connexion à soi-même par les nonces envoyés. | `self_connection_is_detected` |
| I-3 | Info | RPC : le délai s'appliquait à chaque lecture ; un client local envoyant sa requête octet par octet occupait une place des heures. | Délai **total** de 10 s par requête. | `slow_client_hits_total_deadline` |
| I-4 | Info | Avec les contrôles de débordement, des montants absurdes renvoyés par un nœud malveillant (`--rpc`) pouvaient faire planter le CLI (aucun risque pour les fonds). | Sommes saturées. | revue |
| I-5 | Info | Pendant la synchronisation, les mêmes blocs étaient téléchargés auprès de plusieurs pairs. | Un bloc en cours de téléchargement auprès d'un pair n'est pas redemandé à un autre. | tests réseau |

**Zones jugées saines par le relecteur** : validation de consensus (aucune
création de monnaie possible, maturité, doubles dépenses, unicité des
récompenses, genèse inutilisable) ; non-malléabilité (empreinte de signature,
`verify_strict`, Merkle séparé par domaine) ; réorganisations, marquage des
blocs invalides et choix de chaîne (test différentiel) ; LWMA ; arithmétique
256 bits et encodage ; absence d'inversion d'ordre des verrous ; RPC (jeton en
temps constant, recréé à chaque démarrage, fichier `0600` sous Unix, aucun
accès depuis un navigateur) ; portefeuille (vecteurs officiels, paramètres
Argon2id bornés avant dérivation, données associées qui empêchent toute
dégradation des paramètres, écriture atomique, frais exacts, index de monnaie
enregistré avant diffusion).

---

## Constats de la première passe (tous corrigés)

### Consensus et chaîne

| ID | Gravité | Constat | Correction | Test de non-régression |
|---|---|---|---|---|
| C-01 | Élevée | **Fin de l'émission** : une sortie de montant nul est interdite et une récompense devait avoir au moins une sortie. Une fois la subvention tombée à 0 (33ᵉ division), **aucun bloc sans frais n'était plus valide** : la chaîne se serait figée faute de transactions. En regtest (division tous les 150 blocs), dès le bloc 4 950. | Une récompense peut n'avoir aucune sortie ; le mineur n'en crée pas quand subvention + frais = 0. Un transfert doit toujours avoir au moins une sortie. | `blocks_remain_valid_after_emission_ends` |
| C-02 | Moyenne | **Division 256 bits fausse** pour un diviseur > 2²⁵⁵ (le reste intermédiaire dépassait 256 bits) : calcul du travail faussé pour les cibles extrêmes. Non atteignable avec les paramètres actuels, mais une règle de consensus ne doit pas reposer sur cette chance. | Bit de débordement géré, soustraction modulaire. | `overflow_detection`, `arithmetic_matches_u128` |
| C-03 | Moyenne | Après une réorganisation **ratée** (branche invalide découverte en cours de route), le compte rendu de changement listait le sommet d'origine comme « nouveau » : relais et mempool travaillaient sur une information fausse. | Effet net calculé bloc par bloc. | `invalid_heavier_branch_is_rejected` |
| C-04 | Moyenne | **Mémoire lors d'une réorganisation très profonde** : tous les blocs retirés et ajoutés restaient en mémoire jusqu'à la fin de la bascule ; avec des blocs pleins, plantage possible — y compris au redémarrage, qui rejoue la même bascule (plantage en boucle). | Seules les empreintes sont gardées pendant la bascule ; compte rendu limité à 1 000 blocs / 64 Mio par liste, signalé comme tronqué ; la mempool revérifie tout. | `huge_reorg_report_is_bounded` |
| C-05 | Faible | Testnet : un horodatage pouvait avancer de 720 s, plus que la recommandation de l'auteur de LWMA (≈ N·T/20 = 360 s) : le bloc suivant devenait environ 20 % plus facile. | Limite ramenée à 360 s sur le testnet (le mainnet, avec N = 120, reste à 720 s). | `future_time_limit_suits_lwma` |
| C-06 | Faible | Une fenêtre LWMA ou un temps de bloc nul dans les paramètres provoquerait une division par zéro. | Garde-fou vérifié par les tests pour tous les réseaux. | `retarget_parameters_are_usable` |

### Réseau et dénis de service

| ID | Gravité | Constat | Correction | Test |
|---|---|---|---|---|
| P-01 | Élevée | **Remplissage du disque** : tout bloc à en-tête valide était écrit sur le disque, même sur une branche sans chance de gagner ; un attaquant pouvait produire à bas prix de longues branches partant de loin dans le passé. | Les branches lointaines et plus légères restent dans une réserve bornée en mémoire (100 000 blocs / 64 Mio) et ne sont écrites qu'en revenant dans la course ; les feuilles invalides puis les moins lourdes sont oubliées en premier (voir I-1, R-02). | `side_branch_stays_in_memory_until_it_wins`, `parked_pool_forgets_cheapest_branches_first`, `blocks_already_on_disk_are_never_parked`, `lighter_side_branch_never_reaches_the_disk` |
| P-02 | Moyenne | Une seule adresse IP pouvait occuper toutes les connexions entrantes. | 4 au plus par IP (ou /64 IPv6), hors boucle locale. | revue |
| P-03 | Moyenne | Isolement (« eclipse ») : rien n'empêchait toutes les connexions sortantes d'aller vers des adresses voisines contrôlées par une même personne. | Une seule connexion sortante par groupe d'adresses. | revue |
| P-04 | Moyenne | Limitation de débit mal calibrée : les blocs **demandés** consommaient des jetons (une synchronisation rapide pouvait faire couper un pair honnête), et un bloc non sollicité ne coûtait pas plus qu'un ping. | Bloc demandé : gratuit ; bloc non sollicité : 10 jetons ; transaction : selon ses entrées. | `token_bucket_limits_bursts` |
| P-05 | Moyenne | **Minage paralysé par un flot de transactions** : chaque transaction faisait reconstruire immédiatement le modèle de bloc ; à quelques dizaines par seconde, le mineur ne faisait presque plus que reconstruire. | Nouveau sommet : immédiat ; nouvelles transactions : au plus toutes les 2 secondes. | revue |
| P-06 | Faible | Connexion à soi-même détectée, mais l'adresse restait au carnet et était retentée sans fin. | Adresse retirée du carnet. | `self_connection_is_detected` |

### Stockage

| ID | Gravité | Constat | Correction | Test |
|---|---|---|---|---|
| S-01 | Élevée | **Troncature destructrice** : un octet abîmé au milieu du dernier fichier de blocs faisait tronquer le fichier à cet endroit, supprimant **tous les blocs suivants, même intacts** (jusqu'à 128 Mio) ; pour un mineur seul, perte définitive de sa chaîne. | Seule une vraie fin interrompue (rien d'intact après le dégât) est tronquée ; sinon le nœud refuse de démarrer et explique quoi faire, sans rien détruire. | `corruption_is_detected`, `damaged_last_record_is_truncated`, `truncated_tail_is_recovered` |
| S-02 | Faible | Un fichier de blocs raccourci par un autre programme pendant l'exécution provoquait une soustraction qui déborde. | Position hors fichier traitée comme une donnée abîmée. | revue |

### Portefeuille

| ID | Gravité | Constat | Correction | Test |
|---|---|---|---|---|
| W-01 | Élevée (latent) | **Graine de repli nulle** : en cas d'erreur de dérivation, le code se rabattait sur une graine de 64 zéros. Impossible aujourd'hui, mais une évolution du code aurait pu, sans bruit, dériver toutes les clés d'une graine connue de tous : **vol de tous les fonds**. | Fonction de dérivation sans erreur possible ; plus aucune valeur de repli. | `official_vectors` |
| W-02 | Moyenne | **Frais aberrants** : une faute de frappe sur `--fee-rate` (ex. 1 000 000) coûtait environ 2 KYNO, sans confirmation avec `--yes`. | Plafond de frais à 1 KYNO (500 fois les frais de la plus grande transaction au tarif par défaut). | `absurd_fee_is_refused` |
| W-03 | Faible | Boucle infinie possible à la création d'un portefeuille si le générateur aléatoire échouait (erreur ignorée). | Erreur remontée, tirage de deux positions distinctes sans boucle. | `verification_positions_are_distinct_and_in_range` |
| W-04 | Faible | Hygiène mémoire : phrase normalisée, tampon de l'entropie et ligne saisie non effacés (ou copiés lors d'un agrandissement). | Tampons effacés à la libération, capacité réservée d'avance. | `normalize_cleans_spacing_and_case` |
| W-05 | Faible | Réponses du nœud limitées à 64 Mio : une adresse de minage utilisée près d'un an aurait rendu le solde illisible. | Limite portée à 512 Mio. | revue |
| W-06 | Info | Un test de falsification du portefeuille échouait une fois sur 256. | Modification toujours effective. | `tampering_is_detected` |

### Construction et dépendances

| ID | Gravité | Constat | Correction |
|---|---|---|---|
| B-01 | Faible | En version optimisée, un débordement arithmétique « bouclait » silencieusement. | `overflow-checks = true` (effet sur le minage dans la marge de mesure). |
| B-02 | Info | Fonction inutilisée de `zeroize` tirant une dépendance de plus. | Retirée ; version bornée à `~1.8`. |
| B-03 | Info | Compteur de hachages du mineur faux d'un lot (affichage). | Comptage exact. |
| B-04 | Info | Aucune surveillance des avis de sécurité dans l'intégration continue. | `cargo audit` à chaque modification et chaque semaine. |

---

## Risques résiduels et limites connues

| ID | Gravité | Description | Que faire | Bloquant mainnet ? |
|---|---|---|---|---|
| R-01 | Moyenne (était **Critique**) | **Attaque des 51 %.** Avec SHA-256, la puissance des machines spécialisées de Bitcoin se louait à l'heure pour un coût dérisoire. **Traité en 0.4.0** : RandomX (testnet à partir du bloc 21 000, mainnet dès la genèse) ôte presque tout avantage à ces machines, et le travail SHA-256 d'avant l'activation ne peut plus l'emporter sur le travail RandomX (V-1). Reste : un attaquant qui réunit la majorité de la puissance RandomX (processeurs, ou puissance louée sur une place de marché RandomX, voir R-16) peut réécrire les blocs postérieurs au dernier point de contrôle signé (atténuation de la 0.2.0), miner la plupart des nouveaux blocs et retarder des transactions. | Éprouver RandomX sur le testnet ; nombreuses confirmations au début ; surveiller la puissance de calcul. | Non (plus bloquant, à éprouver) |
| R-02 | Élevée (mainnet) | **Pas de synchronisation « en-têtes d'abord » ni de travail minimal codé en dur.** La protection actuelle (branches lointaines en mémoire) limite le remplissage du disque sans l'empêcher : (1) une branche partant de moins de 1 000 blocs sous le sommet (ou n'importe quelle branche pendant les 1 000 premiers blocs d'un réseau) est écrite sur le disque même plus légère, avant la vérification complète de ses transactions (blocs jusqu'à 1 Mo), et sa difficulté peut être abaissée en étirant ses horodatages (LWMA) : en simulation, 338 blocs pour le prix de 18 blocs honnêtes. Sur un petit testnet public, quelqu'un avec un PC ou une carte graphique peut donc remplir le disque des nœuds ; (2) un attaquant peut occuper la réserve en mémoire (bornée) ; (3) une branche concurrente partant de plus de 1 000 blocs sous le sommet et plus grosse que la réserve n'est pas adoptée automatiquement. **Atténué en 0.2.0** sur un réseau à points de contrôle : toute branche qui part d'avant le dernier point est refusée d'emblée, sans être stockée ; le point (1) ne concerne plus que les branches partant après lui. | En-têtes d'abord + travail minimal par version, puis éventuellement points de contrôle : `docs/LANCEMENT.md` phase 3, `docs/DECISIONS.md` § 11 ; difficulté de genèse du mainnet adaptée à la puissance attendue. | **Oui** |
| R-03 | Moyenne | État en **mémoire** : ensemble UTXO, index des transactions confirmées (≈ 50 octets par transaction, pour toujours) et données d'annulation (1 000 blocs) ; chaîne **revalidée entièrement** à chaque démarrage. Remplir des blocs de petites transactions fait grandir la mémoire pour le prix des frais (environ 0,01 KYNO par bloc plein au tarif minimal). | Base de données sur disque (état UTXO, annulations, index facultatif). | Recommandé |
| R-04 | Moyenne | Pas d'éviction des connexions entrantes : quelques adresses (8 × 4) peuvent occuper toutes les places entrantes. Les connexions **sortantes** ne sont pas touchées. | Politique d'éviction (groupes d'adresses, ancienneté, utilité). | Recommandé |
| R-05 | Moyenne | Pas de chaînes de transactions non confirmées, ni de remplacement (RBF) ou de « l'enfant paie pour le parent » : une transaction aux frais trop bas peut rester en attente ; la monnaie rendue n'est dépensable qu'après confirmation. | Gestion des dépendances dans la mempool. | Recommandé |
| R-06 | Faible | Pas de nœuds d'amorçage : un nouveau nœud doit recevoir `--addnode`. | Seeds (adresses fixes ou DNS). | Oui (pratique) |
| R-07 | Faible | Ctrl+C arrête le nœud sans procédure d'arrêt. Les données restent sûres (chaque bloc est synchronisé sur disque, une fin interrompue est réparée), mais le carnet d'adresses n'est enregistré que toutes les 5 minutes. | Gestion du signal d'arrêt ; en attendant, préférer `kynovara-cli stop`. | Non |
| R-08 | Info | **Windows vérifié** le 29 octobre 2026 sur ton PC (Rust 1.97.0, outils C++ Build Tools 2022) : compilation réussie et 159 tests sur 159 réussis (le 160ᵉ, une mesure de mémoire, ne tourne que sous Linux). Pas encore d'essai réseau entre plusieurs PC Windows. **0.4.0 vérifiée sous Windows par l'intégration continue** (`windows-latest`, 21 novembre 2026 : compilation, lint et tests réussis), mais pas sur ton PC : RandomX y exige en plus CMake (fourni avec Build Tools, composant « Outils CMake C++ pour Windows », mais absent du `PATH`), et le 20 novembre 2026 Smart App Control a bloqué l'exécution du script de compilation de `randomx-rs`. | Compiler dans Docker (Linux) sur ton PC ; la CI compile et teste sous Windows à chaque modification. | Non |
| R-09 | Faible | Le RPC est en HTTP sans chiffrement : sûr en boucle locale (défaut), mais `--rpc-allow-remote` ferait circuler le jeton en clair. | Ne pas exposer le RPC ; tunnel SSH si besoin. | Non |
| R-10 | Faible | Mempool pleine : chaque nouvelle transaction déclenche un tri complet (coût processeur limité par les frais à payer). Pas de plafond d'envoi de blocs anciens aux pairs (un pair peut redemander toute la chaîne ; il paie la même bande passante). | Index trié ; plafond d'envoi quotidien. | Non |
| R-11 | Faible | Sous Windows, les fichiers sensibles (jeton RPC, portefeuille) ne reçoivent pas de droits d'accès explicites : ils héritent de ceux du dossier, d'où l'importance de garder les données dans `%APPDATA%` (défaut). | Droits explicites réservés au propriétaire. | Non |
| R-12 | Info | L'effacement des secrets en mémoire est « au mieux » (copies internes des bibliothèques, saisie masquée du mot de passe, mémoire d'échange du système). Aucun `Cargo.lock` n'est livré (créé à la première compilation ; les bibliothèques de consensus sont de toute façon épinglées). | Chiffrement du disque ; conserver le `Cargo.lock` généré. | Non |
| R-13 | Moyenne | **Code C++ dans le consensus** : RandomX (tevador/RandomX, avec compilateur à la volée) est du code non vérifié par Rust ; il a été audité en 2019 par Trail of Bits, X41, Kudelski Security et Quarkslab pour Monero. `randomx-rs` ne vérifie pas que la création d'une machine virtuelle a réussi, et RandomX ignore un refus de rendre la mémoire exécutable (`mprotect`) : le processus planterait au premier calcul au lieu de renvoyer une erreur. Kynovara essaie donc le JIT dans un processus fils au démarrage et passe à l'interpréteur s'il échoue (V-2) ; une machine virtuelle nulle (mémoire épuisée) ferait encore planter le nœud. La compilation exige CMake et un compilateur C++. | Suivre les versions de RandomX et de `randomx-rs` ; proposer les deux vérifications en amont. | Recommandé |
| R-14 | Moyenne | **Coût de la vérification** : une preuve RandomX coûte ≈ 15 ms (quelques microsecondes en SHA-256), calculée sous le verrou principal du nœud. Toutes les règles bon marché passent avant, et un pair qui envoie un bloc à la preuve fausse est banni, mais chaque nouvelle connexion peut en faire calculer une. Un orphelin RandomX ne peut être vérifié qu'à l'arrivée de son parent : il n'est gardé que s'il est proche du sommet (RX-1), au plus 2 par parent manquant (V-3), dans une réserve bornée à 64 blocs et 16 Mo ; son expéditeur n'est pas pénalisé s'il s'avère faux. Le premier bloc d'une nouvelle clé coûte ≈ 0,3 s (préparation du cache). | En-têtes d'abord (R-02) ; vérifier hors du verrou principal ; préparer le cache de la clé suivante à l'avance. | Recommandé |
| R-15 | Faible | **Mineur intégré en mode léger** : ≈ 60 essais/s par fil, environ dix fois moins que le mode rapide (jeu de données de 2 Gio partagé entre les fils, comme XMRig), car `randomx-rs` ne permet pas ce partage sans code `unsafe`. Un attaquant équipé d'un mineur optimisé a donc un avantage par processeur sur les mineurs honnêtes. Mémoire : ≈ 256 Mio par fil de minage, plus 512 Mio au plus pour la vérification. | Mode rapide (modification de `randomx-rs` en amont, ou exception `unsafe` relue) ; interface pour mineurs externes (XMRig). | Avant le mainnet |
| R-16 | Info | **RandomX tel quel** (configuration de Monero) : la puissance des mineurs de Monero, ou celle louée sur les places de marché RandomX, peut être dirigée vers Kynovara sans rien modifier. C'est le risque R-01 en bien moindre mesure (puissance de processeurs, moins concentrée et moins louable que SHA-256). | Décider avant le mainnet d'une variante propre à Kynovara (autre sel Argon2 : `docs/DECISIONS.md` § 4) ; points de contrôle signés en attendant. | À décider |
| R-17 | Info | **Relecture « de confiance » du disque** (V-7) : au démarrage, la preuve RandomX des blocs relus n'est pas recalculée, ce qui suppose des règles identiques à l'écriture et à la relecture. Un changement futur de la hauteur d'activation ou du calendrier des clés, ou un dossier `blocks` copié depuis une autre machine, ferait relire des blocs RandomX jamais vérifiés par ces règles. | Enregistrer une empreinte des règles dans le dossier de données et tout revérifier si elle change ; en attendant, ne jamais copier un dossier `blocks` d'une source non fiable. | Avant le mainnet |

---

## Avant un lancement public

1. R-01 : fait en 0.4.0 (RandomX), à éprouver sur le testnet et à faire
   relire de façon indépendante. Implémenter R-02 (en-têtes d'abord, travail
   minimal), puis R-03 à R-06, R-14 et R-15 ; trancher R-16.
2. Faire tourner un testnet public plusieurs semaines, avec des attaques
   simulées (minage égoïste, horodatages extrêmes, branches concurrentes,
   pairs malveillants).
3. Fuzzing guidé par la couverture (`cargo fuzz`) des décodeurs et de la
   validation, pendant plusieurs jours.
4. **Audit externe** par une firme spécialisée en sécurité des chaînes de
   blocs, puis vérification des corrections.
5. Volet juridique et fiscal (AMF, CANAFE, fiscalité) : `docs/LANCEMENT.md`.

## Refaire les vérifications

Depuis le dossier du projet, une commande à la fois :

```powershell
cargo clippy --workspace --all-targets -- -D warnings
```

```powershell
cargo test --workspace --release
```

```powershell
cargo install cargo-audit
```

```powershell
cargo audit
```
