# Security Policy

## Local Data & Privacy

`ais-shell` operates locally:

- It reads session histories and SQLite databases produced by local AI coding CLIs (`~/.omp`, `~/.claude`, `~/.codex`, `~/.pi`, `~/.gemini`) without modifying them.
- Live plan usage checks are **opt-in** for background polling and run through the local `ais` launcher / official CLIs (`claude`, `codex`, `agy`).
- Programs running inside PTY panes cannot silently read your clipboard (`OSC 52` load is disabled), and `OSC 52` clipboard writes can be toggled off in Settings (`Terminal -> Allow OSC 52 clipboard writes`).

## Reporting a Vulnerability

If you discover a security issue (for example, escape-sequence handling, path quoting, or command invocation), please open a private security advisory on GitHub or contact the maintainer directly rather than posting exploit details in a public issue before a fix is available.
