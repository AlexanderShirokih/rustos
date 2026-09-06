# Окружение и зависимости

## Rust toolchain

Toolchain зафиксирован в `rust-toolchain.toml` до конкретной даты nightly; `rustup` подхватывает его автоматически при запуске `cargo`.

Компоненты: `rust-src`, `llvm-tools-preview`, `clippy`, `rustfmt`,
`rust-analyzer`. Через `rustup` ставится только `aarch64-unknown-none`, под
которым идёт ядро. Таргет программ userspace задан спекой в `targets/`, и `core`
со `std` собираются для него из исходников компонента `rust-src`
([workspace.md](workspace.md#таргеты-сборки)).

## Host-зависимости

| Инструмент            | Когда нужен                                      |
|-----------------------|--------------------------------------------------|
| `cargo-binutils`      | `cargo objcopy` внутри `xtask build`             |
| `qemu-system-arm`     | запуск QEMU и `xtask qemu-test`                  |
| `gcc` / `libc6-dev`   | линкер для host-сборок и build-скриптов          |
| `mkbootimg`           | сборка `android_boot_v1` / `android_boot_v2`     |
| `similarity-rs`       | `xtask similarity` в составе `xtask check`       |

Минимальная установка для QEMU-разработки:

```bash
rustup component add llvm-tools-preview
cargo install cargo-binutils
sudo apt-get install -y gcc libc6-dev qemu-system-arm
```

Для Android boot image дополнительно нужен `mkbootimg` из Android platform-tools.

Для проверки на дублирование кода можно установить `similarity-rs`. Нужную
версию держит `similarity.toml`, а готовую строку установки печатает сама
команда:

```bash
cargo xtask similarity
```

CI использует Docker-образ, описанный в `Dockerfile`.

## rust-analyzer

Дерево разбито на workspace по профилям сборки
([workspace.md](workspace.md#профили-сборки)), а rust-analyzer открывает один.
Остальные подключаются настройкой `linkedProjects`:

```json
{
  "rust-analyzer.linkedProjects": [
    "Cargo.toml",
    "tools/Cargo.toml",
    "kernel/platform/Cargo.toml",
    "lib/std-runtime/Cargo.toml",
    "user/Cargo.toml",
    "fuzz/Cargo.toml"
  ]
}
```
