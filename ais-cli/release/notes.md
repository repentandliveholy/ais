**ais 0.4.2**

- **Windows:** live Codex usage works again. ais picked npm's extensionless `codex` shell shim, which Windows cannot run ("Could not launch official CLI"); it now runs `codex.cmd` / `.exe` from PATHEXT.
- **Claude usage with Claude Code 2.1:** limits are read again (the /usage dialog footer changed), ais no longer runs Claude in the home folder (Claude never keeps trust there), and a `.claude.json` holding a cut emoji no longer hides the trusted folders.
- **Clearer Usage errors:** each failed source says what to do (`codex login`, `/login` in claude, sign in to Antigravity, press R); a CLI you don't use shows as "not installed" instead of an error.
- **Smoother scrolling:** wheel and trackpad bursts move by the number of events (up to 5 rows per frame) instead of one row per 40 ms, so fast scrolling no longer stalls.

Install or upgrade (installed copies update themselves within a day; `ais --update` updates now):

```sh
curl -fsSL https://ais.repentandliveholy.org/install.sh | sh      # macOS / Linux
irm https://ais.repentandliveholy.org/install.ps1 | iex          # Windows PowerShell
```
