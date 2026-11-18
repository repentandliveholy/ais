#!/usr/bin/env python3
"""Write dist/ais-gui-latest.json: one GUI binary per target with its SHA-256.

The updater verifies the Ed25519 signature of these exact bytes
(ais-gui-latest.json.sig) before trusting any URL or checksum inside.
"""
import hashlib
import json
import sys
import time
from pathlib import Path

REPO = "https://github.com/repentandliveholy/ais"


def main() -> None:
    version, tag, folder = sys.argv[1], sys.argv[2], Path(sys.argv[3])
    assets = {}
    for path in sorted(folder.glob("ais-gui-*")):
        if path.name.startswith("ais-gui-latest"):
            continue
        target = path.name[len("ais-gui-"):].removesuffix(".exe")
        assets[target] = {
            "url": f"{REPO}/releases/download/v{tag}/{path.name}",
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            "size": path.stat().st_size,
        }
    if not assets:
        raise SystemExit("no ais-gui-* binaries in " + str(folder))
    manifest = {
        "version": version,
        "date": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "notes": f"{REPO}/releases/tag/v{tag}",
        "assets": assets,
    }
    (folder / "ais-gui-latest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    main()
