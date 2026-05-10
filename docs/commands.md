# Команды

## Быстрая справка

| Действие | Команда |
|---|---|
| Host-тесты | `cargo test --workspace --exclude drivers-aarch64 --exclude hal-aarch64` |
| Host-clippy | `cargo clippy --workspace --exclude drivers-aarch64 --exclude hal-aarch64` |
| AArch64 clippy | `cargo clippy --workspace --exclude xtask --target aarch64-unknown-none` |
| Форматирование | `cargo fmt --all --check` |
| Аудит зависимостей | `cargo deny check` |
| Сборка QEMU | `cargo xtask build devices/spec/qemu-aarch64.yaml` |
| QEMU integration tests | `cargo xtask qemu-test --timeout 60` |
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
cargo test --workspace --exclude drivers-aarch64 --exclude hal-aarch64
```

Без `--exclude` host-тесты падают на платформах, где не компилируется
aarch64 inline assembly из `drivers-aarch64` и `hal-aarch64`.

QEMU integration tests:

```bash
cargo xtask qemu-test --timeout 60
```

## Аудит зависимостей

```bash
cargo deny check
```

Команда требует установленный `cargo-deny`; конфигурация — `deny.toml`.
