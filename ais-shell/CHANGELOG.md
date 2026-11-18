# Changelog

All notable changes to `ais-shell` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.1] - 2026-11-18

### Fixed
- Embedded multi-resolution Windows `.ico` resource (`assets/icon.ico`) into `ais-gui.exe` and set Start Menu shortcut icon in `install.ps1`.
- Added `--install-desktop` for Linux (`ais.desktop` + `ais.png`) invoked automatically by `install.sh`.
- Matched Windows mouse-wheel scroll speed (`SPI_GETWHEELSCROLLLINES`) for `ScrollArea` lists and terminal scrollback.
- Fixed the GUI brand logo (`paint_logo`) to render the curved `J`-arc, gradient flow, round joins, and launch play mark accurately.
- Fixed the `UiIcon::Refresh` vector icon geometry and update button icon.

## [0.2.0] - 2026-11-17

### Added
- Native GPU-rendered multi-pane workspace (`eframe` / `egui` + `alacritty_terminal` + `portable-pty`) for OMP, Claude Code, Codex, agy, Gemini CLI, Pi, and login shells.
- Live agent status cards across spaces, space tabs, command palette with fuzzy search, scrollback search (`⌘F`), file browser with `@mention` insertion, and alerts dock.
- Plan usage limits panel (`ais --usage --json`) with opt-in background live refresh, cached snapshots, and per-window indicators.
- 15 UI color themes (each with light and dark modes, plus `System` auto-switching) and 8 terminal palettes.
- Rebindable keyboard shortcuts with conflict checking against fixed navigation bindings.
- File logging and panic hook diagnostics, settings backup on parse failure, and window/layout persistence.
- macOS `.app` bundle script (`bundle.sh`) with self-contained 1024 px icon, path-prefix remapping, and deployment target `11.0`.
