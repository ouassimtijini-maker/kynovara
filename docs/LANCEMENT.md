# Feuille de route vers le réseau principal

Kynovara 0.1 est une base solide pour le **développement local** et un
**réseau de test**. Voici le chemin jusqu'à un réseau principal avec de la
vraie valeur, dans l'ordre. Ne saute pas d'étape : une erreur de consensus
après le lancement peut coûter de l'argent réel à des gens réels.

---

## Phase 1 — Développement local (maintenant)

- Prendre le code en main avec `regtest` (voir `DEMARRAGE.md`).
- Miner sur le `testnet` avec ton PC, observer l'ajustement de difficulté.
- Faire tourner 2 ou 3 nœuds (même PC, puis plusieurs PC du réseau local).

## Phase 2 — Trancher les décisions

Toutes les questions de [DECISIONS.md](DECISIONS.md), en particulier :
le **nom** (conflit existant), l'**algorithme de preuve de travail**, la
**politique monétaire**, la **licence**.

## Phase 3 — Renforcement technique

Travaux recommandés avant un réseau public (voir aussi `AUDIT.md`,
section « Limites connues ») :

1. Changer d'algorithme de preuve de travail si décidé (et le tester).
2. **Synchronisation par en-têtes d'abord** (« headers-first ») : télécharger
   et vérifier les en-têtes avant les blocs, en parallèle depuis plusieurs
   pairs, avec un **travail minimal** codé en dur à chaque version (une chaîne
   d'en-têtes plus légère est ignorée sans rien stocker). Plus rapide, et c'est
   la vraie protection contre les branches bon marché (la réserve en mémoire
   actuelle, voir `SPECIFICATION.md` § 8, est une mesure intermédiaire).
3. Stockage de l'état UTXO dans une **base de données** sur disque (aujourd'hui
   en mémoire et reconstruit au démarrage, ce qui ralentira avec le temps).
4. **Chaînes de transactions non confirmées** dans la mempool (dépenser une
   monnaie rendue avant sa confirmation).
5. **Nœuds d'amorçage** (adresses ou DNS) pour que les nouveaux nœuds trouvent
   le réseau.
6. Tests de charge, tests de « fuzzing » prolongés, intégration continue sous
   Windows et Linux, **compilations reproductibles** et binaires signés.
7. Un **explorateur de blocs** (site web en lecture seule) — l'API RPC
   (`RPC.md`) suffit pour le construire.

## Phase 4 — Audit externe professionnel

Le rapport `AUDIT.md` est un audit interne sévère, pas un audit indépendant.
Avant tout lancement public avec de la valeur, faire auditer par une firme
spécialisée en sécurité blockchain :

- les règles de consensus (`kynovara-core`) ;
- le réseau pair-à-pair (dénis de service, isolement d'un nœud) ;
- le portefeuille (gestion des clés).

Corriger, puis faire vérifier les corrections.

## Phase 5 — Testnet public

> **En cours depuis le 31 octobre 2026** (version 0.3.2) : serveur public
> `51.222.143.109:19733` inscrit comme nœud d'amorçage ; guide
> [REJOINDRE.md](REJOINDRE.md).

- Règles de communication, partout (code, site, réseaux sociaux) : « réseau
  de test, jetons sans valeur, aucune vente, aucun investissement » ; ne rien
  vendre, ne rien prévendre, ne rien promettre sur la valeur future
  ([AVIS.md](../AVIS.md)).
- Publier le code et des binaires ; inviter des mineurs volontaires.
- Le faire vivre **plusieurs semaines ou mois** : mises à jour, redémarrages,
  réorganisations, pannes de mineurs, attaques simulées (minage égoïste,
  horodatages extrêmes).
- Mettre en place un programme de récompense pour les failles (« bug bounty »).

## Phase 6 — Juridique et fiscal (Québec / Canada)

**Je ne suis pas avocat : ce qui suit sert à préparer la discussion avec un
professionnel, pas à la remplacer.**

- **Valeurs mobilières (AMF)** : vendre des pièces, en réserver aux
  fondateurs, lever des fonds ou promettre des profits peut constituer un
  placement de valeurs mobilières, soumis à l'Autorité des marchés financiers.
  Un lancement équitable sans vente ni promesse réduit ce risque, sans
  l'éliminer.
- **Lutte contre le blanchiment (CANAFE)** : échanger des cryptomonnaies pour
  le compte d'autrui ou en transférer pour des clients peut exiger une
  inscription comme entreprise de services monétaires.
- **Fiscalité** : les revenus de minage et les gains en cryptomonnaies sont
  imposables ; en parler avec un comptable (et décider si l'activité passe
  par ta société par actions).
- **Marque** : nom et logo (voir `DECISIONS.md`, point 1).
- **Communication** : ne jamais présenter la monnaie comme un placement ni
  promettre une valeur.

## Phase 7 — Cérémonie de lancement (genèse du réseau principal)

1. Geler le code (version publiée, étiquetée, auditée).
2. Choisir la date et le message de genèse (ex. titre d'un journal du jour).
3. Miner la genèse :

   ```powershell
   .\kynovara-cli.exe --network mainnet genesis --message "<message>"
   ```

4. Reporter les valeurs affichées (horodatage, nonce, message, empreinte)
   dans `MAINNET` (`crates/kynovara-core/src/params.rs`), ainsi que la cible de
   la genèse choisie.
5. Relancer toute la suite de tests, compiler les binaires de version,
   publier leurs empreintes SHA-256 et les signer.
6. Démarrer les nœuds d'amorçage, puis annoncer publiquement le code, les
   binaires et l'empreinte de la genèse, **avant** l'heure du premier bloc.

## Phase 8 — Après le lancement

- Surveiller la chaîne (réorganisations profondes, puissance de calcul,
  version des nœuds).
- Plan d'incident : qui décide, comment publier un correctif d'urgence,
  comment prévenir les utilisateurs.
- Les règles de consensus ne changent que par une mise à jour annoncée,
  testée sur le testnet, et adoptée par le réseau.

## À chaque nouvelle version

1. Sur le nœud signataire : `kynovara-cli checkpoint export`.
2. Ajouter la ligne obtenue à `release_checkpoints` du réseau
   (`crates/kynovara-core/src/params.rs`).
3. Lancer tous les tests, puis publier la version (programmes signés).
