# Окружение и зависимости

## Rust toolchain

Toolchain зафиксирован в `rust-toolchain.toml` до конкретной даты nightly; `rustup` подхватывает его автоматически при запуске `cargo`.

Компоненты: `rust-src`, `llvm-tools-preview`, `clippy`, `rustfmt`, `rust-analyzer`. Target:
`aarch64-unknown-none` — под ним идёт ядро. Программы userspace идут под таргет Molecular OS,
он задан спекой `targets/aarch64-unknown-molecular.json` и ставить его через `rustup` нечем:
`core` и `std` для него собираются из исходников компонента `rust-src`.

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
