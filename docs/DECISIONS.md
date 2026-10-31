# Décisions à prendre avant le lancement

Plusieurs valeurs du code sont **provisoires**. Elles fonctionnent pour le
développement et le testnet, mais doivent être tranchées **avant de créer le
bloc de genèse du réseau principal**, car la plupart ne pourront plus changer
ensuite sans scinder le réseau.

Pour chaque point : la valeur actuelle, les options, une recommandation et
l'endroit où la modifier.

---

## 1. Le nom — choisi : « Kynovara »

**Choix** : « Kynovara », le nom de ta compagnie Kynovara Labs (aucune crypto
de ce nom trouvée le 29 octobre 2026).

**Attention** : une crypto qui porte le nom d'une compagnie la désigne comme
émettrice. Si des acheteurs comptent sur le travail de la compagnie pour faire
monter la valeur, l'AMF pourrait y voir un placement réglementé, et la
réputation de la compagnie sera liée à celle de la crypto. Faire valider ce
choix par un avocat en valeurs mobilières, et vérifier que la marque couvre
les logiciels et services financiers (OPIC au Canada, USPTO, EUIPO).

**Où** : textes et messages (`README.md`, `docs/`, messages de genèse dans
`crates/kynovara-core/src/params.rs`, `USER_AGENT` dans `crates/kynovara-node/src/node.rs`).

## 2. Symbole (ticker) — choisi : `KYNO`

Aucun jeton `KYNO` trouvé le 29 octobre 2026 (attention à la ressemblance
avec `KYO`, déjà utilisé). Revérifier juste avant le lancement.

**Où** : `TICKER` dans `crates/kynovara-core/src/amount.rs`.

## 3. Préfixes d'adresse — choisis : `kyn` / `tkyn` / `rkyn`

Courts, lisibles, distincts par réseau. Le préfixe du réseau principal ne
pourra **jamais** changer après le lancement.

**Où** : champ `hrp` des trois réseaux dans `crates/kynovara-core/src/params.rs`.

## 4. Algorithme de preuve de travail — actuellement SHA-256 double ⚠️

C'est la décision la plus importante pour la sécurité.

**Risque** : SHA-256 est l'algorithme de Bitcoin. Des machines spécialisées
(ASIC) immensément plus puissantes que n'importe quel PC existent, et leur
puissance se **loue** à l'heure. Un réseau naissant en SHA-256 peut être pris
d'assaut à peu de frais (« attaque des 51 % » : réécriture de l'historique,
double dépense). Miner sur un PC n'a pas non plus de sens face aux ASIC.

**Options** :

| Option | Pour | Contre |
|---|---|---|
| Garder SHA-256d | simple, éprouvé, déjà fait | vulnérable à la puissance louée ; minage sur PC inutile |
| SHA-256d + **minage fusionné** avec Bitcoin (AuxPoW) | hérite d'une partie de la sécurité de Bitcoin | complexe ; dépend de la bonne volonté des pools Bitcoin |
| **RandomX** (algorithme de Monero, conçu pour les processeurs) | minage sur PC équitable, résistant aux ASIC | intégration plus lourde (bibliothèque C++) ; puissance processeur aussi louable, en moindre mesure |
| Algorithme original | aucune puissance louable au départ | risque cryptographique ; les cartes graphiques finissent par dominer |

**Recommandation** : pour une monnaie minée sur des PC, **RandomX** est le
choix le plus cohérent ; à intégrer et tester sur le testnet avant le
lancement. Quel que soit le choix, prévoir : nombre de confirmations conseillé
élevé au début (ex. 30 blocs pour de gros montants), surveillance des
réorganisations profondes.

**Où** : fonctions de hachage d'en-tête (`crates/kynovara-core/src/block.rs`)
et de minage (`crates/kynovara-core/src/miner.rs`), puis
`docs/SPECIFICATION.md`.

## 5. Politique monétaire — actuellement 50 KYNO/bloc, ÷2 tous les ~4 ans, ~105 M au total

**Question ouverte** : quand la récompense devient presque nulle, seuls les
frais rémunèrent les mineurs. Certaines monnaies (Monero) ajoutent une
**émission de queue** perpétuelle et faible pour garder un budget de sécurité.

**Options** : garder le plafond strict (modèle Bitcoin), ou ajouter une
récompense minimale permanente (ex. 0,5 KYNO par bloc).

**Où** : `block_subsidy` et les paramètres `initial_subsidy` /
`halving_interval` dans `crates/kynovara-core/src/params.rs`, `MAX_MONEY` dans
`amount.rs`.

## 6. Pré-extraction et financement — actuellement **aucune**

Le bloc de genèse ne donne rien à personne : lancement équitable, le plus
simple juridiquement et le plus crédible.

**Attention** : réserver des pièces aux fondateurs, en vendre avant ou après
le lancement, ou promettre une hausse de valeur peut faire de la monnaie une
**valeur mobilière** au sens de la loi (AMF au Québec). Avis juridique
obligatoire avant toute décision de ce type (voir `LANCEMENT.md`).

**Engagement actuel (31 octobre 2026)** : aucune vente, aucune prévente,
aucune réserve, aucune promesse de valeur ; affiché dans `AVIS.md`, le
`README.md` et les programmes eux-mêmes sur les réseaux de test.

## 7. Temps de bloc et taille de bloc — actuellement 2 minutes, 1 Mo

Raisonnables pour une jeune chaîne. Un temps plus court augmente les blocs
concurrents (« orphelins ») ; une taille plus grande alourdit les nœuds.

**Où** : `target_spacing` (`params.rs`), `MAX_BLOCK_SIZE` (`block.rs`).

## 8. Paramètres de difficulté du réseau principal

- **Cible de la genèse** (difficulté des 120 premiers blocs) : à régler selon
  la puissance attendue au lancement. Trop facile : les premiers blocs partent
  en quelques secondes ; trop difficile : démarrage lent. La difficulté
  s'ajuste ensuite automatiquement.
- **Fenêtre LWMA** (120 blocs) et **cible minimale** (`pow_limit`).

**Où** : `MAINNET` dans `crates/kynovara-core/src/params.rs`.

## 9. Bloc de genèse — date et message

Le message de genèse est gravé pour toujours. La tradition (Bitcoin) est d'y
mettre le titre d'un journal du jour : cela prouve publiquement que personne
n'a miné en secret avant cette date.

**Comment** : `kynovara-cli --network mainnet genesis --message "…"` puis
reporter les valeurs affichées dans `params.rs` (voir `LANCEMENT.md`).

## 10. Licence du code — choisie : MIT ou Apache-2.0, au choix

**Choix (31 octobre 2026)** : double licence **MIT OR Apache-2.0**, comme Rust
et la plupart de ses bibliothèques : réutilisation libre, clause « sans
garantie », et protection en matière de brevets avec Apache 2.0. Titulaire
indiqué : « The Kynovara Developers » (peut être remplacé par le nom de ta
compagnie dans `LICENSE-MIT`).

**Où** : `LICENSE-MIT`, `LICENSE-APACHE` et le champ `license` de
`Cargo.toml` (section `[workspace.package]`).

## 11. Réorganisations profondes et « finalité »

**Actuellement** : règle de Nakamoto pure — la chaîne valide la plus lourde
gagne, quelle que soit la profondeur de la réorganisation. Les branches plus
légères sont gardées en mémoire (bornée) et jamais écrites sur le disque tant
qu'elles ne dépassent pas la chaîne active (voir `SPECIFICATION.md`, § 8).

**Options** :

| Option | Pour | Contre |
|---|---|---|
| Garder la règle actuelle | aucune scission possible : deux groupes séparés se réunissent automatiquement | une attaque des 51 % peut réécrire un historique profond |
| Profondeur maximale de réorganisation (ex. 100 blocs) | protège les échanges contre les réécritures profondes | deux groupes séparés plus longtemps que la limite ne se réunissent **jamais** (scission permanente) |
| Points de contrôle (« checkpoints ») publiés à chaque version | fige l'historique ancien sans risque de scission automatique | dépend de la confiance envers ceux qui publient les versions |

**Choisi (29 octobre 2026)** : **points de contrôle signés** par le
fondateur pendant la phase de lancement (`SPECIFICATION.md`, § 14), avec une
clé maîtresse hors ligne, une clé de signature déléguée pour ~90 jours, un
arrêt de sécurité en cas de contradiction et une fin programmée. Un point
signé à chaque bloc limite toute réécriture au dernier bloc.

**Encore à décider pour le mainnet** :
- la hauteur de fin (`checkpoint_sunset_height`) — par exemple 2 ans, à
  annoncer publiquement dès le lancement ;
- la cérémonie de création de la clé maîtresse (ordinateur jamais connecté,
  deux copies papier en lieux sûrs, éventuellement plusieurs détenteurs) ;
- **une seule** machine signataire : deux nœuds qui signent chacun leur sommet
  peuvent produire des points contradictoires et déclencher une alerte ;
- garder en plus la synchronisation par en-têtes d'abord et le travail
  minimal (R-02), et conseiller aux échanges d'attendre qu'un paiement soit
  couvert par un point de contrôle.

**Où** : `crates/kynovara-core/src/chain.rs`.

## 12. Autres valeurs à figer

| Valeur | Actuelle | Où |
|---|---|---|
| Type de pièce SLIP-44 (dérivation des clés) | 9733 (provisoire) — faire enregistrer un numéro officiel auprès de SatoshiLabs (SLIP-0044) | `slip44_coin_type`, `params.rs` |
| Octets magiques P2P | `KYNM` | `magic`, `params.rs` |
| Ports | 9733 / 9734 | `params.rs` |
| Nœuds d'amorçage (« seeds ») | testnet : `51.222.143.109:19733` (serveur du fondateur, OVH Beauharnois, depuis la version 0.3.2) ; mainnet : aucun | ajouter d'autres serveurs, chez d'autres hébergeurs et d'autres personnes, avant le lancement du réseau principal |
| Gouvernance des mises à jour | — | qui publie les versions, comment une règle de consensus évolue |

## 13. Période de rodage (aucun envoi au début) — choisi : date fixe

**Choix (30 octobre 2026)** : pendant une période fixée d'avance, on mine et
les récompenses s'accumulent, mais **aucun envoi** n'est possible. À la
hauteur de fin, Kynovara devient une crypto normale, automatiquement.
Personne, pas même le fondateur, ne peut avancer cette date.

**Pourquoi** : pas de paiements donc pas de double dépense à craindre, pas de
marché ni de plateforme d'échange tant que le réseau est jeune ; un bug grave
découvert pendant le rodage peut être corrigé, voire la chaîne relancée, sans
léser personne.

**Valeurs** : testnet — premiers envois au bloc 6 000 (environ une semaine
après la décision) ; mainnet — **à trancher au lancement**, proposition
21 600 blocs (≈ 30 jours). Pour durer plus longtemps, il faudra le décider
**avant** la genèse : prolonger ensuite exige une nouvelle version adoptée par
tous les nœuds, et raccourcir aussi.

**Limites** : on ne peut pas empêcher quelqu'un de vendre hors du réseau ses
24 mots ou un fichier de portefeuille (déconseillé, et facile à frauder pour le
vendeur) ; la période de rodage ne remplace ni les points de contrôle ni
l'avis juridique (je ne suis pas avocat).

**Où** : `transfers_unlock_height` dans `crates/kynovara-core/src/params.rs`.
