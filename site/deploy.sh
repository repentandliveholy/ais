#!/usr/bin/env bash
# Publish ais.repentandliveholy.org: the landing page, installers for both
# `ais` (GUI) and `ais-cli` (CLI), and the signed update manifests from the
# latest GitHub release.
#
#   ./site/deploy.sh               # DEPLOY_HOST=ovh DEPLOY_TARGET_DIR=/srv/ais-site
#
# Caddy on the host serves the folder:
#   ais.repentandliveholy.org {
#       root * /srv/ais-site
#       file_server
#       header /releases/* Cache-Control "no-cache"
#       header /install* Cache-Control "no-cache"
#   }
set -euo pipefail

cd "$(dirname "$0")/.."
HOST="${DEPLOY_HOST:-ovh}"
TARGET_DIR="${DEPLOY_TARGET_DIR:-/srv/ais-site}"
OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT

cp -R site/. "$OUT/"
rm -f "$OUT/deploy.sh"
cp ais-cli/release/install.sh ais-cli/release/install.ps1 ais-cli/release/install-cli.sh ais-cli/release/install-cli.ps1 "$OUT/"
mkdir -p "$OUT/releases"
gh release download --repo repentandliveholy/ais --pattern '*latest.json*' --dir "$OUT/releases" --clobber

# Never publish manifests the shipped binaries would reject.
(cd ais-cli && cargo run --quiet --example release_sign -- verify release-key.pub "$OUT/releases/latest.json")
(cd ais-cli && cargo run --quiet --example release_sign -- verify release-key.pub "$OUT/releases/ais-gui-latest.json")

echo "==> Deploying to $HOST:$TARGET_DIR"
rsync -rlz --delete --chmod=Du=rwx,Dgo=rx,Fu=rw,Fgo=r "$OUT/" "$HOST:$TARGET_DIR/"

echo "==> Checking https://ais.repentandliveholy.org"
for path in / /install.sh /install.ps1 /install-cli.sh /install-cli.ps1 /releases/latest.json /releases/latest.json.sig /releases/ais-gui-latest.json /releases/ais-gui-latest.json.sig; do
  curl -sSf -o /dev/null -w "%{http_code} $path\n" "https://ais.repentandliveholy.org$path"
done
