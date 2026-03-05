# AGENTS.md

Машиночитаемый индекс документации проекта RustOS Mobile.

> Детальная документация расположена в [`docs/`](docs/). Этот файл служит точкой входа и кратким справочником для автоматизированных агентов и сопровождающих проекта.

## Cursor Cloud specific instructions

### Обзор проекта

RustOS Mobile — bare-metal aarch64 ядро на Rust (`#![no_std]`, Edition 2024). 14 крейтов в workspace, `xtask` — хост-инструмент сборки.

-> Подробнее: [`docs/overview.md`](docs/overview.md)

### Команды

| Действие | Команда |
|---|---|
| Тесты (host) | `cargo test --workspace --exclude drivers-aarch64 --exclude arch-aarch64 --exclude kernel` |
| Линтинг (host) | `cargo clippy --workspace --exclude drivers-aarch64 --exclude arch-aarch64 --exclude kernel` |
| Линтинг (aarch64) | `cargo clippy --workspace --exclude xtask --target aarch64-unknown-none` |
| Форматирование | `cargo fmt --all --check` |
| Аудит зависимостей | `cargo deny check` |
| Сборка (QEMU) | `cargo xtask build devices/spec/qemu-aarch64.yaml` |
| Запуск (QEMU) | `cargo xtask build devices/spec/qemu-aarch64.yaml --run` |

-> Подробнее: [`docs/commands.md`](docs/commands.md)

### Окружение и подводные камни

- `cargo test` без `--exclude` падает — aarch64 inline assembly не компилируется на x86_64.
- QEMU работает бесконечно — оборачивайте в `timeout`.
- Toolchain зафиксирован в `rust-toolchain.toml` (nightly). `rustup` подберёт нужную версию автоматически.
- `Cargo.lock` в `.gitignore` — зависимости разрешаются заново.
- Системная зависимость: `qemu-system-arm` для boot-тестирования.

-> Подробнее: [`docs/environment.md`](docs/environment.md)

## Документация

| Документ | Описание |
|---|---|
| [`docs/overview.md`](docs/overview.md) | Обзор проекта, граф крейтов, workspace |
| [`docs/commands.md`](docs/commands.md) | Сборка, тестирование, линтинг, запуск |
| [`docs/environment.md`](docs/environment.md) | Настройка окружения, зависимости, подводные камни |
| [`docs/architecture.md`](docs/architecture.md) | Принципы архитектуры: слои, трейты, newtype, compile-time гарантии |
| [`docs/code-style.md`](docs/code-style.md) | Стиль кода: структура файлов, комментарии, `no_std` |
| [`docs/testing.md`](docs/testing.md) | Тестирование: интеграционные/юнит-тесты, именование, паттерны |
