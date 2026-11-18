#!/usr/bin/env bash
# Publish ais.repentandliveholy.org: the landing page, both installers and the
# signed update manifest of the latest GitHub release.
#
#   ./site/deploy.sh               # DEPLOY_HOST=ovh DEPLOY_TARGET_DIR=/srv/ais-site
#
# Caddy on the host serves the folder:
#   ais.repentandliveholy.org {
#       root * /srv/ais-site
#       file_server
#       header /releases/* Cache-Control "no-cache"
#       header /install.* Cache-Control "no-cache"
#   }
set -euo pipefail

cd "$(dirname "$0")/.."
HOST="${DEPLOY_HOST:-ovh}"
TARGET_DIR="${DEPLOY_TARGET_DIR:-/srv/ais-site}"
OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT

cp -R site/. "$OUT/"
rm -f "$OUT/deploy.sh"
cp ais-cli/release/install.sh ais-cli/release/install.ps1 "$OUT/"
mkdir -p "$OUT/releases"
gh release download --repo repentandliveholy/ais --pattern 'latest.json*' --dir "$OUT/releases" --clobber

# Never publish a manifest the shipped binaries would reject.
(cd ais-cli && cargo run --quiet --example release_sign -- verify release-key.pub "$OUT/releases/latest.json")

echo "==> Deploying to $HOST:$TARGET_DIR"
rsync -rlz --delete --chmod=Du=rwx,Dgo=rx,Fu=rw,Fgo=r "$OUT/" "$HOST:$TARGET_DIR/"

echo "==> Checking https://ais.repentandliveholy.org"
for path in / /install.sh /install.ps1 /releases/latest.json /releases/latest.json.sig; do
  curl -sSf -o /dev/null -w "%{http_code} $path\n" "https://ais.repentandliveholy.org$path"
done
