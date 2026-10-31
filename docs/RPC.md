# Interface RPC du nœud

Le nœud expose une petite API HTTP + JSON, utilisée par `kynovara-cli`. Tu
peux t'en servir pour tes propres outils (tableau de bord, explorateur,
automatisations…).

- Adresse : `http://127.0.0.1:19734` (testnet), `:29734` (regtest).
- Authentification : en-tête `Authorization: Bearer <jeton>`. Le jeton est le
  contenu du fichier `.cookie` du dossier de données
  (`%APPDATA%\Kynovara\testnet\.cookie`), recréé à chaque démarrage.
- Les montants sont donnés en unités (`value`, entier) et en KYNO (`amount`,
  texte).
- Erreurs : code HTTP 4xx/5xx et `{"error": {"code": "...", "message": "..."}}`.

> Le RPC permet de piloter le minage et d'arrêter le nœud : ne l'expose
> jamais sur le réseau, et ne donne jamais le jeton à un tiers.

## Exemple PowerShell

Une commande pour lire le jeton :

```powershell
$jeton = Get-Content "$env:APPDATA\Kynovara\testnet\.cookie"
```

Puis une requête :

```powershell
Invoke-RestMethod -Uri http://127.0.0.1:19734/v1/info -Headers @{ Authorization = "Bearer $jeton" }
```

## Points d'accès

| Méthode | Chemin | Corps | Réponse |
|---|---|---|---|
| GET | `/v1/info` | — | réseau, hauteur, sommet, difficulté, pairs, mempool, minage, fin du rodage (`transfers_unlock_height`), point de contrôle |
| GET | `/v1/block/<empreinte>` | — | bloc détaillé (en-tête, transactions) |
| GET | `/v1/block-at/<hauteur>` | — | bloc de la chaîne active à cette hauteur |
| GET | `/v1/tx/<txid>` | — | `status` = `mempool` ou `confirmed`, confirmations, transaction |
| POST | `/v1/utxos` | `{"addresses": ["tkyn1…", …]}` (≤ 2 000) | pièces non dépensées (`mature`, `spent_in_mempool`) et entrées en attente |
| POST | `/v1/tx` | `{"hex": "<transaction encodée>"}` | `{"txid": …}` ou 409 si refusée |
| GET | `/v1/mempool` | — | nombre, taille, txid |
| GET | `/v1/peers` | — | pairs connectés |
| POST | `/v1/peers` | `{"address": "IP:PORT"}` | ajoute un pair à joindre |
| GET | `/v1/mining` | — | état du minage |
| POST | `/v1/mining/start` | `{"address": "tkyn1…", "threads": 4}` | démarre le minage |
| POST | `/v1/mining/stop` | `{}` | arrête le minage |
| POST | `/v1/generate` | `{"count": 10, "address": "rkyn1…"}` | regtest : mine immédiatement |
| POST | `/v1/stop` | `{}` | arrête le nœud |
| GET | `/v1/checkpoint` | — | points de contrôle : dernier, en attente, alerte, clé de signature, révocations, fin |
| POST | `/v1/checkpoint` | `{"hex": "<point encodé>"}` | soumet un point (ex. décision de la clé maîtresse) |
| POST | `/v1/revocation` | `{"hex": "<révocation encodée>"}` | révoque une clé de signature |
| POST | `/v1/signer` | `{"secret": "<hex 32 o>", "delegation": "<hex>"}` | installe la clé de signature du nœud (local uniquement) |
