# Команды

## Быстрая справка

| Действие | Команда |
|---|---|
| Юнит/интеграционные тесты (host) | `cargo test --workspace --exclude drivers-aarch64 --exclude arch-aarch64 --exclude kernel` |
| Линтинг (host-крейты) | `cargo clippy --workspace --exclude drivers-aarch64 --exclude arch-aarch64 --exclude kernel` |
| Линтинг (aarch64-крейты) | `cargo clippy --workspace --target aarch64-unknown-none` |
| Форматирование | `cargo fmt --all --check` |
| Аудит зависимостей | `cargo deny check` |
| Сборка ядра (QEMU) | `cargo xtask build devices/spec/qemu-aarch64.yaml` |
| Запуск в QEMU | `cargo xtask build devices/spec/qemu-aarch64.yaml --run` |
| Отладка в QEMU | `cargo xtask build devices/spec/qemu-aarch64.yaml --debug` |
| Сборка для устройства | `cargo xtask build devices/spec/<device>.yaml` |

## Сборка

Сборка выполняется через `cargo xtask`:

```bash
# QEMU (формат binary)
cargo xtask build devices/spec/qemu-aarch64.yaml

# Xiaomi Redmi Note 7 (формат android_boot_v1)
cargo xtask build devices/spec/xiaomi-lavender.yaml
```

### Результат

- `binary` -> `target/build/kernel.bin`
- `android_boot_v1`, `android_boot_v2` -> `target/build/boot.img`

## Тестирование

```bash
# Все host-совместимые крейты
cargo test --workspace --exclude drivers-aarch64 --exclude arch-aarch64 --exclude kernel
```

> **Важно:** без `--exclude` сборка упадёт — крейты с inline assembly aarch64 не компилируются на x86_64.

## Линтинг

```bash
# Host-крейты
cargo clippy --workspace --exclude drivers-aarch64 --exclude arch-aarch64 --exclude kernel

# aarch64-крейты (требуется target aarch64-unknown-none)
cargo clippy --workspace --target aarch64-unknown-none
```

## Форматирование

```bash
# Проверка (CI-режим)
cargo fmt --all --check

# Автоформатирование
cargo fmt --all
```

Конфигурация в `rustfmt.toml` (корень проекта).

## Аудит зависимостей

```bash
cargo deny check
```

Конфигурация в `deny.toml`. Проверяет лицензии, уязвимости, дубликаты зависимостей.

## Запуск и отладка

```bash
# Сборка + запуск (выполняет команды из секции 'run' в YAML)
cargo xtask build devices/spec/qemu-aarch64.yaml --run

# Сборка + отладка (выполняет команды из секции 'debug' в YAML)
cargo xtask build devices/spec/qemu-aarch64.yaml --debug
```

QEMU работает бесконечно (ядро входит в цикл таймера). В CI/автоматизации оборачивайте в `timeout`:

```bash
timeout 10 cargo xtask build devices/spec/qemu-aarch64.yaml --run
```
