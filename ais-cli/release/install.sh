#!/bin/sh
# ais · AI Sessions installer for macOS and Linux (installs `ais` CLI + `ais-gui` desktop app).
#
#   curl -fsSL https://ais.repentandliveholy.org/install.sh | sh
set -eu

REPO="https://github.com/repentandliveholy/ais"
DIR="${AIS_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${AIS_VERSION:-latest}"

say() { printf '%s\n' "$*"; }
die() { printf 'ais install: %s\n' "$*" >&2; exit 1; }

case "$(uname -s)" in
  Darwin)
    os=apple-darwin
    cli_os=apple-darwin
    ;;
  Linux)
    os=unknown-linux-gnu
    cli_os=unknown-linux-musl
    ;;
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

cli_target="$arch-$cli_os"
cli_asset="ais-$cli_target"
gui_target="$arch-$os"
gui_asset="ais-gui-$gui_target"

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

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'
  else shasum -a 256 "$1" | awk '{print $1}'; fi
}

verify_and_install() {
  asset="$1"
  dest="$2"
  tmp_file="$tmp/$asset"
  say "Downloading ${asset}…"
  fetch "$base/$asset" "$tmp_file" || return 1
  expected=$(awk -v f="$asset" '$2 == f || $2 == "*" f { print $1 }' "$tmp/SHA256SUMS")
  [ -n "$expected" ] || die "$asset is missing from SHA256SUMS"
  actual=$(sha256_of "$tmp_file")
  [ "$expected" = "$actual" ] || die "checksum mismatch for $asset"
  chmod 755 "$tmp_file"
  mv -f "$tmp_file" "$dest"
  return 0
}

tmp=$(mktemp -d 2>/dev/null || mktemp -d -t ais)
trap 'rm -rf "$tmp"' EXIT INT TERM

fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || die "could not download SHA256SUMS"
mkdir -p "$DIR"

verify_and_install "$cli_asset" "$DIR/ais" || die "no ais CLI build for $cli_target in this release"
say "Installed $("$DIR/ais" --version) to $DIR/ais"

if verify_and_install "$gui_asset" "$DIR/ais-gui"; then
  say "Installed $("$DIR/ais-gui" --version) to $DIR/ais-gui"
else
  say "Note: prebuilt GUI binary ($gui_asset) is not available for $gui_target; use ais (CLI) or build from ais-shell/."
fi

case ":$PATH:" in
  *":$DIR:"*) ;;
  *) say ""
     say "Add $DIR to PATH, for example:"
     say "  echo 'export PATH=\"$DIR:\$PATH\"' >> ~/.$(basename "${SHELL:-sh}")rc" ;;
esac
say "Run: ais (CLI) or ais-gui (GUI)"
