#!/usr/bin/env python3
"""Write dist/latest.json: one raw binary per target with its SHA-256.

The updater verifies the Ed25519 signature of these exact bytes (latest.json.sig)
before it trusts any URL or checksum inside.
"""
import hashlib
import json
import sys
import time
from pathlib import Path

REPO = "https://github.com/repentandliveholy/ais"


def main() -> None:
    version, folder = sys.argv[1], Path(sys.argv[2])
    assets = {}
    for path in sorted(folder.glob("ais-*")):
        if path.name.startswith(("ais-gui-", "ais-cli-")):
            continue
        target = path.name[len("ais-"):].removesuffix(".exe")
        assets[target] = {
            "url": f"{REPO}/releases/download/v{version}/{path.name}",
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            "size": path.stat().st_size,
        }
    if not assets:
        raise SystemExit("no ais-* binaries in " + str(folder))
    manifest = {
        "version": version,
        "date": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "notes": f"{REPO}/releases/tag/v{version}",
        "assets": assets,
    }
    (folder / "latest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    main()
