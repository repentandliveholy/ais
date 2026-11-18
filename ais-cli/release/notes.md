**ais 0.4.3**

- **Smooth, fast scrolling:** the list no longer re-filters, re-sorts and re-groups every session on each scroll step. A frame on the Projects tab dropped from ~45 ms to ~1–3 ms (Sessions: ~5 ms → <1 ms) on a history of ~1800 sessions, so wheel and trackpad scrolling no longer stalls or jumps, even on a busy machine.

Install or upgrade (installed copies update themselves within a day; `ais --update` updates now):

```sh
curl -fsSL https://ais.repentandliveholy.org/install.sh | sh      # macOS / Linux
irm https://ais.repentandliveholy.org/install.ps1 | iex          # Windows PowerShell
```
