# Обзор проекта

RustOS Mobile — bare-metal ядро для aarch64, написанное на Rust (`#![no_std]`, Edition 2024).

## Целевые платформы

- QEMU `virt` (основная среда разработки и CI)
- Raspberry Pi 5
- Xiaomi Redmi Note 7 (Qualcomm SDM660)

## Граф крейтов

Зависимости образуют DAG. Стрелки — направление зависимости:

```
arch/aarch64            <- точка входа, платформенный код
  ├── kernel            <- основная логика ядра
  ├── drivers-aarch64   <- платформенные драйверы
  ├── memory            <- управление физической памятью
  ├── aarch64-paging    <- таблицы страниц aarch64
  └── arch-common       <- общие типы архитектуры
        └── drivers-common         <- интерфейс драйверов
        └── drivers-common-aarch64 <- aarch64-специфичные типы драйверов
              ├── io              <- ввод/вывод, ByteSink, Writer
              ├── collections    <- Vec, IntervalSet (no_std)
              ├── util           <- утилиты
              ├── fdt            <- парсер Device Tree
              └── log            <- логирование (klog)
```

## Workspace

Полный список крейтов определён в корневом `Cargo.toml`:

| Крейт | Путь | Описание |
|---|---|---|
| `arch-aarch64` | `arch/aarch64` | Точка входа, boot, платформа |
| `aarch64-paging` | `arch/aarch64-paging` | Таблицы страниц ARM64 |
| `arch-common` | `arch/common` | Общие типы архитектуры |
| `collections` | `collections` | `Vec`, `IntervalSet` (`no_std`) |
| `drivers-common` | `drivers/common` | Трейты и типы драйверов |
| `drivers-common-aarch64` | `drivers/common-aarch64` | aarch64-специфичные типы драйверов |
| `drivers-aarch64` | `drivers/aarch64` | Реализации драйверов (PL011, GIC) |
| `kernel` | `kernel` | Основная логика ядра |
| `fdt` | `fdt` | Парсер Flattened Device Tree |
| `io` | `io` | `ByteSink`, `Writer`, форматирование |
| `log` | `log` | Макросы логирования (`klog`) |
| `memory` | `memory` | Frame allocator, bitmap |
| `util` | `util` | Утилиты |
| `xtask` | `xtask` | Хост-инструмент сборки (CLI) |
