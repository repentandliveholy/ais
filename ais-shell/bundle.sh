#!/bin/sh
# Builds dist/ais.app (macOS) with the ais icon. Usage: ./bundle.sh [--install]
set -eu

if [ "$(uname -s)" != "Darwin" ]; then
  echo "bundle.sh builds a macOS .app bundle and must be run on macOS." >&2
  exit 1
fi

cd "$(dirname "$0")"

TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-11.0}"
REMAP_FLAGS="--remap-path-prefix=${HOME}=. --remap-path-prefix=$(pwd)=."
export RUSTFLAGS="${RUSTFLAGS:-} ${REMAP_FLAGS}"

cargo build --release

BIN="target/release/ais-shell"
if strings "$BIN" | grep -F -q "${HOME}"; then
  echo "Error: release binary contains build-machine home path (${HOME})" >&2
  exit 1
fi

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP="dist/ais.app"
rm -rf "$APP" "dist/ais.app.dSYM"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/ais"

if [ -d "target/release/ais_shell.dSYM" ]; then
  cp -RL "target/release/ais_shell.dSYM" "dist/ais.app.dSYM"
elif [ -d "target/release/ais-shell.dSYM" ]; then
  cp -RL "target/release/ais-shell.dSYM" "dist/ais.app.dSYM"
fi

ICONSET="$TMP_DIR/ais.iconset"
mkdir -p "$ICONSET"
SRC="assets/icon-1024.png"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$SRC" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z "$double" "$double" "$SRC" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/ais.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>ais</string>
  <key>CFBundleDisplayName</key><string>ais · AI Sessions</string>
  <key>CFBundleIdentifier</key><string>io.github.repentandliveholy.ais</string>
  <key>CFBundleExecutable</key><string>ais</string>
  <key>CFBundleIconFile</key><string>ais.icns</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
  <key>NSPrincipalClass</key><string>NSApplication</string>
  <key>NSHumanReadableCopyright</key><string>Copyright © 2026 repentandliveholy. MIT License.</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key><string>Folder</string>
      <key>CFBundleTypeRole</key><string>Viewer</string>
      <key>LSHandlerRank</key><string>Alternate</string>
      <key>LSItemContentTypes</key>
      <array>
        <string>public.folder</string>
      </array>
    </dict>
  </array>
</dict>
</plist>
PLIST

# Bundle the native `ais` indexer so the app needs no Python or PATH setup.
if [ -f ../ais-cli/Cargo.toml ]; then
  (cd ../ais-cli && cargo build --release)
  cp ../ais-cli/target/release/ais "$APP/Contents/Resources/ais"
fi

IDENTITY="${CODESIGN_IDENTITY:--}"
codesign --force --sign "$IDENTITY" "$APP" >/dev/null 2>&1 || true
echo "Built $APP (macOS >= ${MACOSX_DEPLOYMENT_TARGET})"
if [ "${1:-}" = "--install" ]; then
  rm -rf "/Applications/ais.app"
  cp -R "$APP" /Applications/
  echo "Installed /Applications/ais.app"
fi
