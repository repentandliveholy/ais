#!/bin/sh
# ais · AI Sessions installer for macOS and Linux.
#
#   curl -fsSL https://ais.repentandliveholy.org/install.sh | sh
#
# Downloads the binary for this OS/CPU from the latest GitHub release, checks
# its SHA-256 against the release SHA256SUMS and installs it to
# $AIS_INSTALL_DIR (default ~/.local/bin). ais then keeps itself up to date.
set -eu

REPO="https://github.com/repentandliveholy/ais"
DIR="${AIS_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${AIS_VERSION:-latest}"

say() { printf '%s\n' "$*"; }
die() { printf 'ais install: %s\n' "$*" >&2; exit 1; }

case "$(uname -s)" in
  Darwin) os=apple-darwin ;;
  Linux) os=unknown-linux-musl ;;
  *) die "unsupported OS $(uname -s); on Windows use install.ps1" ;;
esac
case "$(uname -m)" in
  arm64 | aarch64) arch=aarch64 ;;
  x86_64 | amd64) arch=x86_64 ;;
  *) die "unsupported CPU $(uname -m)" ;;
esac
# Rosetta: prefer the native arm64 build on Apple Silicon.
if [ "$os" = apple-darwin ] && [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
  arch=aarch64
fi
target="$arch-$os"
asset="ais-$target"

if [ "$VERSION" = latest ]; then
  base="$REPO/releases/latest/download"
else
  base="$REPO/releases/download/v${VERSION#v}"
fi

fetch() {
  if command -v curl >/dev/null 2>&1; then curl -fsSL "$1" -o "$2"
  elif command -v wget >/dev/null 2>&1; then wget -q "$1" -O "$2"
  else die "curl or wget is required"; fi
}

tmp=$(mktemp -d 2>/dev/null || mktemp -d -t ais)
trap 'rm -rf "$tmp"' EXIT INT TERM

say "Downloading ${asset}…"
fetch "$base/$asset" "$tmp/ais" || die "no build for $target in this release"
fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || die "could not download SHA256SUMS"

expected=$(awk -v f="$asset" '$2 == f || $2 == "*" f { print $1 }' "$tmp/SHA256SUMS")
[ -n "$expected" ] || die "$asset is missing from SHA256SUMS"
if command -v sha256sum >/dev/null 2>&1; then actual=$(sha256sum "$tmp/ais" | awk '{print $1}')
else actual=$(shasum -a 256 "$tmp/ais" | awk '{print $1}'); fi
[ "$expected" = "$actual" ] || die "checksum mismatch for $asset"

mkdir -p "$DIR"
chmod 755 "$tmp/ais"
mv -f "$tmp/ais" "$DIR/ais"
say "Installed $("$DIR/ais" --version) to $DIR/ais"

case ":$PATH:" in
  *":$DIR:"*) ;;
  *) say ""
     say "Add $DIR to PATH, for example:"
     say "  echo 'export PATH=\"$DIR:\$PATH\"' >> ~/.$(basename "${SHELL:-sh}")rc" ;;
esac
say "Run: ais"
