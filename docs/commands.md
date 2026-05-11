# Команды

## Быстрая справка

| Действие | Команда |
|---|---|
| Host-тесты | `cargo test --workspace` |
| Host-clippy | `cargo clippy --workspace` |
| AArch64 clippy | `cargo clippy --workspace --exclude xtask --target aarch64-unknown-none` |
| Форматирование | `cargo fmt --all --check` |
| Аудит зависимостей | `cargo deny check` |
| Сборка QEMU | `cargo xtask build devices/spec/qemu-aarch64.yaml` |
| QEMU integration tests | `cargo xtask qemu-test --timeout 20` |
| Сборка устройства | `cargo xtask build devices/spec/<device>.yaml` |

## Сборка

`xtask build` читает YAML-спеку устройства, выбирает boot feature для
`hal-aarch64`, собирает kernel crate под `aarch64-unknown-none` и
упаковывает результат.

```bash
cargo xtask build devices/spec/qemu-aarch64.yaml
cargo xtask build devices/spec/xiaomi-lavender.yaml
```

Результаты:

| `boot.format` | Артефакт |
|---|---|
| `linux_arm64` | `target/build/kernel.bin` |
| `android_boot_v1` | `target/build/boot.img` |
| `android_boot_v2` | `target/build/boot.img` |
| `uefi` | не реализован |

## Тесты

```bash
cargo test --workspace
```

QEMU integration tests:

```bash
cargo xtask qemu-test --timeout 20
```

## Аудит зависимостей

```bash
cargo deny check
```

Команда требует установленный `cargo-deny`; конфигурация — `deny.toml`.
