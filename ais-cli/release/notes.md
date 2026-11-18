**ais 0.4.2**

- **Windows:** live Codex usage works again. ais picked npm's extensionless `codex` shell shim, which Windows cannot run ("Could not launch official CLI"); it now runs `codex.cmd` / `.exe` from PATHEXT.
- **Smoother scrolling:** wheel and trackpad bursts move by the number of events (up to 5 rows per frame) instead of one row per 40 ms, so fast scrolling no longer stalls.

Install or upgrade (installed copies update themselves within a day; `ais --update` updates now):

```sh
curl -fsSL https://ais.repentandliveholy.org/install.sh | sh      # macOS / Linux
irm https://ais.repentandliveholy.org/install.ps1 | iex          # Windows PowerShell
```
