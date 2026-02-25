# AGENTS.md

## Cursor Cloud specific instructions

### Overview

RustOS Mobile — bare-metal aarch64 kernel written in Rust (`#![no_std]`, Edition 2024). The workspace has 13 crates; see `Cargo.toml` for the full list. The `xtask` crate is the build orchestrator (host-side CLI).

### Key commands

All standard commands are documented in `README.md`. Quick reference:

| Action | Command |
|---|---|
| Unit/integration tests (host) | `cargo test --workspace --exclude drivers-aarch64 --exclude arch-aarch64 --exclude kernel` |
| Lint (host crates) | `cargo clippy --workspace --exclude drivers-aarch64 --exclude arch-aarch64 --exclude kernel` |
| Lint (aarch64 crates) | `cargo clippy --workspace --target aarch64-unknown-none` |
| Build kernel (QEMU) | `cargo xtask build devices/spec/qemu-aarch64.yaml` |
| Run in QEMU | `cargo xtask build devices/spec/qemu-aarch64.yaml --run` |

### Non-obvious caveats

- **`cargo test` без `--exclude` упадёт** — крейты `drivers-aarch64`, `arch-aarch64` и `kernel` содержат aarch64 inline assembly, который не компилируется на x86_64. Используйте `--workspace --exclude drivers-aarch64 --exclude arch-aarch64 --exclude kernel`.
- **QEMU runs indefinitely.** The kernel enters a timer-tick loop after boot. When running in CI or automated testing, wrap with `timeout`, e.g.: `timeout 10 qemu-system-aarch64 ...`
- **No `rust-toolchain.toml`** exists. The toolchain must support Edition 2024 (Rust ≥ 1.85.0). The `aarch64-unknown-none` target, `llvm-tools-preview` component, and `cargo-binutils` must be installed.
- **`Cargo.lock` is gitignored.** Dependencies are resolved fresh on each build.
- **System dependency:** `qemu-system-arm` (provides `qemu-system-aarch64`) must be installed via apt for end-to-end kernel boot testing.
