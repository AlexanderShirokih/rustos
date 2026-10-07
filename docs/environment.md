# Окружение и зависимости

## Rust toolchain

Toolchain, компоненты и bare-таргет зафиксированы в
[rust-toolchain.toml](../rust-toolchain.toml).
`rustup` выбирает их при запуске Cargo из репозитория.

Системный таргет RustOS задан в `targets/`.
`core` и `std` для него собираются из исходников `rust-src`
([workspace.md](workspace.md#таргеты-сборки)).

## Host-зависимости

| Инструмент | Назначение |
|------------|------------|
| `cargo-binutils` | Извлечение бинарного образа через `cargo objcopy` |
| `qemu-system-aarch64` | Запуск и тесты QEMU |
| C/C++ toolchain | Линковка host-программ, сборка зависимостей и fuzz-целей |
| `mkbootimg` | Сборка Android boot image |
| `similarity-rs` | Проверка дублирования |
| `cargo-fuzz` | Сборка и запуск fuzz-целей |

Для QEMU-разработки на Debian:

```bash
cargo install cargo-binutils
sudo apt-get install -y gcc g++ libc6-dev qemu-system-arm
```

`mkbootimg` должен быть доступен в `PATH` при сборке Android-образа.
Нужную версию `similarity-rs` задаёт
[similarity.toml](../similarity.toml); команда
`cargo xtask similarity` печатает строку установки при отсутствии анализатора.

Окружение CI описано в [Dockerfile](../Dockerfile).

## rust-analyzer

Для навигации по всему репозиторию rust-analyzer подключает workspace
профилей сборки через `linkedProjects`:

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

Манифесты и назначение профилей перечислены в
[workspace.md](workspace.md#профили-сборки).
