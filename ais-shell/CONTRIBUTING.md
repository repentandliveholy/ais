# Contributing to ais-shell

Thank you for contributing!

## Prerequisites

- Rust toolchain `1.89` or newer (`rust-version = "1.89"`, Rust 2024 edition).
- On Linux: `libxkbcommon-dev`, `libwayland-dev` / X11 development headers, `libgl-dev`, plus runtime tools `zenity`, `notify-send`, and `xdg-open`.

## Checks

Before opening a pull request, please run:

```sh
cd ais-shell
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## macOS Bundle

```sh
cd ais-shell
./bundle.sh
```
