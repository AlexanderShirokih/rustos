# AGENTS.md

Машиночитаемый индекс документации проекта RustOS Mobile.

> Детальная документация расположена в [`docs/`](docs/). Этот файл служит точкой входа и кратким справочником для автоматизированных агентов и сопровождающих проекта.

**Сборка проекта:** перед сборкой ядра, настройкой toolchain и запуском через `cargo xtask` необходимо прочитать **[`BUILD.md`](BUILD.md)** — там полные инструкции по требованиям, сборке и запуску.

## Cursor Cloud specific instructions

### Обзор проекта

RustOS Mobile — bare-metal aarch64 ядро на Rust (`#![no_std]`, Edition 2024). 14 крейтов в workspace, `xtask` — хост-инструмент сборки.

-> Подробнее: [`docs/overview.md`](docs/overview.md)

### Команды

| Действие | Команда |
|---|---|
| Тесты (host) | `cargo test --workspace --exclude drivers-aarch64 --exclude hal-aarch64` |
| Линтинг (host) | `cargo clippy --workspace --exclude drivers-aarch64 --exclude hal-aarch64` |
| Линтинг (aarch64) | `cargo clippy --workspace --exclude xtask --target aarch64-unknown-none` |
| Форматирование | `cargo fmt --all --check` |
| Аудит зависимостей | `cargo deny check` |
| Сборка (QEMU) | `cargo xtask build devices/spec/qemu-aarch64.yaml` |
| Запуск (QEMU) | `cargo xtask build devices/spec/qemu-aarch64.yaml --run` |
| QEMU integration tests | `cargo xtask build devices/spec/qemu-aarch64-test.yaml --features qemu-tests --run` |

-> Подробнее: [`docs/commands.md`](docs/commands.md)

### Окружение и подводные камни

- `cargo test` без `--exclude` падает — aarch64 inline assembly не компилируется на x86_64.
- QEMU работает бесконечно — оборачивайте в `timeout`.
- Toolchain зафиксирован в `rust-toolchain.toml` (nightly). `rustup` подберёт нужную версию автоматически.
- Системная зависимость: `qemu-system-arm` для boot-тестирования.

-> Подробнее: [`docs/environment.md`](docs/environment.md)

### Правила разработки

- **Никакого `unsafe` в архитектурно-независимом коде.** Запрет автоматизирован: `[workspace.lints.rust] unsafe_code = "deny"`. Платформенные крейты (`hal-*`, `drivers-*-aarch64`, `qemu-test-harness-aarch64`) опциируют разрешение целиком через `#![allow(unsafe_code)]` в корне крейта. В архитектурно-независимых крейтах (`memory`, `main`, `io`, `collections`, `fdt`, `drivers-common`, `qemu-test-harness`) `#![allow(unsafe_code)]` ставится **точечно — на уровне отдельного файла**, только если без `unsafe` обойтись невозможно (kernel-mechanism: allocators, scheduler, MMIO, raw-pointer relocation). Новый файл и новый крейт по умолчанию запрещают `unsafe` — добавление `#![allow]` видно в diff и требует обоснования при ревью.
- **`unsafe`-блоки в архитектурно-независимых крейтах — крайне нежелательны.** Даже точечный `#![allow(unsafe_code)]` — крайняя мера. Прежде чем добавлять `unsafe` в `memory`/`main`/`io`/`collections`/`fdt`/`drivers-common`, нужно показать, что задачу нельзя решить безопасным API; обоснование и `// SAFETY:` обязательны на каждом блоке.
- **Архитектурная независимость по умолчанию.** Механизмы ядра, алгоритмы и структуры данных проектируются так, чтобы максимально не зависеть от платформы. Архитектурно-зависимая часть выносится за интерфейс и сводится к минимуму.
- **Никаких архитектурных деталей в коде и комментариях архитектурно-независимых модулей.** В `memory`/`main`/`io`/`collections`/`fdt`/`drivers-common` не должно быть упоминаний `TTBR0`, `EL1`, `MAIR`, `SCTLR`, `vmalle1`, `aarch64`, `x86`, `cr3` и аналогичных архитектурных терминов — ни в идентификаторах, ни в doc-комментариях. Конкретика должна жить только в реализациях за интерфейсом.
- **Тестирование.** Архитектурно-независимый код должен покрываться meaningful юнит-тестами (не тесты-заглушки). Архитектурно-зависимый остаток по возможности покрывается интеграционными тестами (QEMU).
- **Прогон тестов после крупной задачи — обязателен.** По завершении логически законченной работы (новый функционал, рефакторинг, фикс) агент обязан прогнать host-тесты (`cargo test --workspace --exclude drivers-aarch64 --exclude hal-aarch64`) и QEMU integration tests (`cargo xtask build devices/spec/qemu-aarch64-test.yaml --features qemu-tests --run`). Сдача без прогона = незавершённая работа.
- **Комментарии — по необходимости.** Не писать пространных объяснений того, что и так видно из кода. Комментарий уместен только когда объясняет неочевидное "почему" (инвариант, ограничение, обход бага).
- **Не плодить сущности.** Не вводить новые механизмы, абстракции, трейты и слои без явной необходимости. Три похожих строки лучше преждевременной абстракции.

## Документация

| Документ | Описание |
|---|---|
| [`BUILD.md`](BUILD.md) | Сборка: требования, `cargo xtask`, артефакты, запуск и отладка |
| [`docs/overview.md`](docs/overview.md) | Обзор проекта, граф крейтов, workspace |
| [`docs/commands.md`](docs/commands.md) | Сборка, тестирование, линтинг, запуск |
| [`docs/environment.md`](docs/environment.md) | Настройка окружения, зависимости, подводные камни |
| [`docs/architecture.md`](docs/architecture.md) | Принципы архитектуры: слои, трейты, newtype, compile-time гарантии |
| [`docs/code-style.md`](docs/code-style.md) | Стиль кода: структура файлов, комментарии, `no_std` |
| [`docs/testing.md`](docs/testing.md) | Тестирование: интеграционные/юнит-тесты, именование, паттерны |
| [`docs/user-memory.md`](docs/user-memory.md) | User-VM аллокатор и syscall'ы выделения памяти процессам |
