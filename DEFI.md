# Défi « Casse Kynovara »

> ⚠️ **Réseau de test : les KYNO n'ont aucune valeur.** Ce défi ne comporte
> aucun prix, aucun tirage et aucune récompense, ni en argent ni en KYNO.

On a construit une cryptomonnaie qui ne vaut rien, et on te met au défi de la
casser. Chaque faille trouvée maintenant n'existera plus si un réseau
principal voit le jour.

- **Durée** : dès maintenant et jusqu'au **bloc 25 000** (vers le 4 décembre 2026).
- **En direct** : [tableau du réseau](https://ouassimtijini-maker.github.io/kynovara/).
- **Pour participer** : fais tourner un nœud ([docs/REJOINDRE.md](docs/REJOINDRE.md)), lis le code, attaque.

## Les deux manches

1. **Le siège**, jusqu'au bloc 6 000. Les envois sont encore bloqués. Cibles :
   réécrire l'historique signé (attaque à 51 %), faire accepter un bloc
   invalide, scinder le réseau, faire planter ou geler un nœud à distance,
   faire accepter un envoi avant le bloc 6 000.
2. **La double dépense**, du bloc 6 000 au bloc 25 000. Les envois sont ouverts.
   Cibles : dépenser deux fois les mêmes KYNO, créer des KYNO hors des règles,
   dépenser les KYNO d'un autre portefeuille, faire refuser des transactions
   valides par tout le réseau.

## Ce qui compte

| Gravité | Exemples |
|---|---|
| Critique | réécrire un bloc déjà signé, double dépense confirmée, création de KYNO hors des règles, vol des KYNO d'un portefeuille |
| Haute | bloc invalide accepté par les nœuds, plantage ou gel d'un nœud à distance, scission durable du réseau, envoi accepté avant le bloc 6 000 |
| Moyenne | déni de service qui contourne les protections du nœud (limites de débit, bannissements), fuite d'informations du portefeuille |
| Faible | erreur du portefeuille ou de `kynovara-cli`, documentation fausse qui induit en erreur |

Une faille est **confirmée** quand on peut la reproduire : étapes, journaux,
code de démonstration. Le premier signalement reproductible l'emporte.

## Comment signaler

- **Critique, haute ou moyenne** : en privé, avec
  [Report a vulnerability](https://github.com/ouassimtijini-maker/kynovara/security/advisories/new).
  Jamais en public avant la correction.
- **Faible** : une [issue publique](https://github.com/ouassimtijini-maker/kynovara/issues/new/choose).
- On répond dès que possible. Une faille corrigée est expliquée publiquement,
  avec ton accord, au plus tard 30 jours après la correction.

## Ce qui est permis

Tout ce qui passe par le **protocole Kynovara** (port 19733) : faire tourner
autant de nœuds et de mineurs que tu veux, envoyer des messages pair-à-pair
malformés, miner des chaînes concurrentes, jouer avec les horodatages,
saturer la mempool, et attaquer ton propre nœud de toutes les façons.

## Ce qui est interdit

Chacun de ces gestes disqualifie :

- s'en prendre au serveur public en dehors du port 19733 (SSH, système,
  autres services) ou à son hébergeur ;
- les dénis de service volumétriques, qui inondent la connexion du serveur
  au lieu de tester le logiciel ;
- s'en prendre aux ordinateurs, comptes ou portefeuilles d'autres personnes,
  ou aux comptes du projet (GitHub, courriel) ;
- l'hameçonnage et la manipulation de personnes ;
- vendre, acheter ou échanger des KYNO, ou prétendre le faire ;
- publier une faille critique, haute ou moyenne avant sa correction.

## La reconnaissance

- Chaque faille confirmée inscrit son auteur, sous son nom ou un pseudonyme à
  son choix, au [Mur des casseurs](MUR-DES-CASSEURS.md), avec la date, la
  hauteur du bloc et la gravité.
- Si un réseau principal Kynovara est lancé un jour, les noms du Mur des
  casseurs seront gravés dans son bloc de genèse.
- C'est tout : ni argent, ni KYNO, ni promesse de jetons futurs. La
  reconnaissance n'a aucune valeur monétaire.

## Bon à savoir

- L'historique est protégé par des **points de contrôle signés** par le
  fondateur ([spécification](docs/SPECIFICATION.md), § 14) : c'est justement
  ce qu'on te met au défi de contourner.
- Le réseau peut ralentir, se figer ou être remis à zéro pendant le défi :
  c'est à ça que sert un réseau de test.
- Participation gratuite, sans inscription ni achat.
- Kynovara Labs peut corriger ce règlement ; chaque changement est daté
  ci-dessous.

*Règlement publié le 1er novembre 2026.*

---

## "Break Kynovara" challenge (English)

**Test network: KYNO have no value. No prizes, no draw, no reward in money or
KYNO.** We built a cryptocurrency that is worth nothing, and we dare you to
break it, until block 25,000 (around December 4, 2026). Round 1, until block
6,000: rewrite signed history, get an invalid block accepted, split the
network, crash a node remotely. Round 2, from block 6,000: double-spend,
create coins outside the rules, steal from a wallet. Report critical, high
and medium issues privately with
[Report a vulnerability](https://github.com/ouassimtijini-maker/kynovara/security/advisories/new).
Anything through the Kynovara protocol (port 19733) is fair game; attacking
the server outside that port, its host, other people's machines or accounts,
volumetric denial of service and selling KYNO are not. Confirmed findings put
your name on the [Wall of Breakers](MUR-DES-CASSEURS.md), and in the genesis
block of a Kynovara main network if one is ever launched. Nothing else.
