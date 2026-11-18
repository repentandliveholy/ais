**ais 0.2.1 (GUI) & ais 0.4.5 (CLI)**

- **Windows & Linux app icons:** `ais-gui.exe` now embeds a multi-resolution `.ico` resource (fixing blank icons in Explorer, Start Menu, and Taskbar), and Linux installs register `ais.desktop` + `ais.png` via `--install-desktop`.
- **Windows scrolling:** respects `SPI_GETWHEELSCROLLLINES` for both UI scroll areas and terminal scrollback so mouse-wheel scrolling matches native Windows speed.
- **UI polish:** fixed the in-app `ais` GUI logo geometry/gradient and redesigned the refresh (`↻`) vector icons.

Install **ais** (GUI + CLI):

```sh
curl -fsSL https://ais.repentandliveholy.org/install.sh | sh      # macOS / Linux
irm https://ais.repentandliveholy.org/install.ps1 | iex          # Windows PowerShell
```

Install **ais** CLI only (headless / terminal):

```sh
curl -fsSL https://ais.repentandliveholy.org/install-cli.sh | sh  # macOS / Linux
irm https://ais.repentandliveholy.org/install-cli.ps1 | iex      # Windows PowerShell
```
