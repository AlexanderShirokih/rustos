# Окружение и зависимости

## Rust toolchain

Toolchain зафиксирован в `rust-toolchain.toml` до конкретной даты nightly; `rustup` подхватывает его автоматически при запуске `cargo`.

Компоненты: `rust-src`, `llvm-tools-preview`, `clippy`, `rustfmt`. Target: `aarch64-unknown-none`.

## Host-зависимости

| Инструмент            | Когда нужен                                      |
|-----------------------|--------------------------------------------------|
| `cargo-binutils`      | `cargo objcopy` внутри `xtask build`             |
| `qemu-system-arm`     | запуск QEMU и `xtask qemu-test`                  |
| `gcc` / `libc6-dev`   | линкер для host-сборок и build-скриптов          |
| `mkbootimg`           | сборка `android_boot_v1` / `android_boot_v2`     |

Минимальная установка для QEMU-разработки:

```bash
rustup component add llvm-tools-preview
cargo install cargo-binutils
sudo apt-get install -y gcc libc6-dev qemu-system-arm
```

Для Android boot image дополнительно нужен `mkbootimg` из Android platform-tools.

CI использует Docker-образ, описанный в `Dockerfile`.
