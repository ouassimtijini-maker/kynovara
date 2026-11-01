#!/usr/bin/env bash
# Publie l'état du réseau de test sur GitHub Pages (branche gh-pages), depuis
# le serveur public, toutes les 10 minutes (minuterie systemd kynovara-statut).
#
# - Lecture seule sur le nœud : rien n'est exposé sur Internet, le serveur
#   n'ouvre aucun port de plus ; il pousse un fichier JSON vers GitHub.
# - La clé de déploiement (/root/.ssh/kynovara_statut) n'écrit que dans ce
#   dépôt, et la branche main est protégée : elle ne peut toucher qu'à gh-pages.
# - La branche gh-pages ne garde qu'un seul commit, remplacé à chaque fois.
set -euo pipefail

DEPOT="git@github.com:ouassimtijini-maker/kynovara.git"
CLE=/root/.ssh/kynovara_statut
TRAVAIL=/var/lib/kynovara-statut
FIN_DEFI=25000
export GIT_SSH_COMMAND="ssh -i $CLE -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new"

rpc() { docker exec kynovara-public kynovara-cli --datadir /data rpc "$1"; }

mkdir -p "$TRAVAIL"
if [ ! -d "$TRAVAIL/depot/.git" ]; then
  git clone -q --depth 1 --branch main "$DEPOT" "$TRAVAIL/depot"
else
  git -C "$TRAVAIL/depot" fetch -q --depth 1 origin main
  git -C "$TRAVAIL/depot" reset -q --hard origin/main
fi

info=$(rpc /v1/info)
points=$(rpc /v1/checkpoint)
hauteur=$(jq -r .height <<<"$info")
blocs=$(for i in $(seq 0 11); do
  x=$((hauteur - i))
  [ "$x" -ge 0 ] || break
  rpc "/v1/block-at/$x" | jq -c '{hauteur: .height, bloc: .hash, date: .header.timestamp, transactions: (.transactions | length)}'
done | jq -s .)

SITE="$TRAVAIL/site"
rm -rf "$SITE"
mkdir -p "$SITE"
cp "$TRAVAIL/depot/tableau/index.html" "$SITE/"
cp "$TRAVAIL/depot/logo/kyno-icone.svg" "$SITE/icone.svg"
cp "$TRAVAIL/depot/logo/kyno-carte-sociale.png" "$SITE/carte.png"
touch "$SITE/.nojekyll"
jq -n --argjson i "$info" --argjson c "$points" --argjson b "$blocs" --argjson fin "$FIN_DEFI" '{
  genere: (now | floor), reseau: $i.network, version: $i.version,
  hauteur: $i.height, sommet: $i.tip, date_sommet: $i.tip_time,
  difficulte: $i.difficulty, pairs: $i.peers, deblocage: $i.transfers_unlock_height,
  fin_defi: $fin, point_controle: {hauteur: $c.latest.height, bloc: $c.latest.block},
  alerte: ($c.alert != null), blocs: $b
}' > "$SITE/statut.json"

cd "$SITE"
git init -q -b gh-pages
git add -A
git -c user.name="Kynovara, serveur public" -c user.email="serveur@kynovara.invalid" \
  commit -qm "État du réseau de test"
git push -qf "$DEPOT" gh-pages
