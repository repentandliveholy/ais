**ais is now a single native binary** for macOS (Apple Silicon, Intel), Linux (x86_64, arm64, static) and Windows (x64), rewritten in Rust with the same screens, keys, search syntax, JSON output and history adapters as the Python edition.

Install or upgrade:

```sh
curl -fsSL https://ais.repentandliveholy.org/install.sh | sh      # macOS / Linux
irm https://ais.repentandliveholy.org/install.ps1 | iex          # Windows PowerShell
```

From this version on ais **updates itself**: once a day it checks the Ed25519-signed release manifest and replaces itself after verifying the SHA-256 of the new binary. `ais --update` checks right away; `AIS_NO_UPDATE=1` or `"auto_update": false` turns it off.

The Python edition remains in the repository as an optional `ais-py`.
