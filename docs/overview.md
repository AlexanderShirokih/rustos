# Обзор проекта

RustOS Mobile — bare-metal ядро для aarch64, написанное на Rust (`#![no_std]`, Edition 2024).

## Целевые платформы

- QEMU `virt` (основная среда разработки и CI)
- Raspberry Pi 5
- Xiaomi Redmi Note 7 (Qualcomm SDM660)

## Граф крейтов

Зависимости образуют DAG. Стрелки — направление зависимости:

```
crates/hal-aarch64 <- точка входа, платформенный код
  ├── kernelspace       <- orchestration ядра
  ├── scheduler         <- процессы, потоки, планирование
  ├── kobject           <- kernel objects / IPC
  ├── syscall           <- syscall ABI и dispatch
  ├── userspace         <- user image loader и user-VM planning
  ├── drivers-aarch64   <- платформенные драйверы
  ├── memory            <- управление физической памятью
  ├── aarch64-paging    <- таблицы страниц aarch64
  └── hal-common        <- общие типы архитектуры
        └── drivers-common         <- интерфейс драйверов
        └── drivers-common-aarch64 <- aarch64-специфичные типы драйверов
              ├── io              <- ввод/вывод, ByteSink, Writer
              ├── collections     <- Vec, IntervalSet (no_std)
              ├── util            <- утилиты
              ├── fdt             <- парсер Device Tree
              └── log             <- логирование (klog)
```

## Workspace

Полный список крейтов определён в корневом `Cargo.toml`:

| Крейт | Путь | Описание |
|---|---|---|
| `hal-aarch64` | `crates/hal-aarch64` | Точка входа, boot, платформа |
| `hal-aarch64-asid` | `crates/hal-aarch64-asid` | Управление ASID для aarch64 |
| `hal-aarch64-paging` | `crates/hal-aarch64-paging` | Таблицы страниц aarch64 |
| `hal-common` | `crates/hal-common` | Общие типы архитектуры |
| `collections` | `crates/collections` | `Vec`, `IntervalSet` (`no_std`) |
| `drivers-common-aarch64` | `crates/drivers-common-aarch64` | aarch64-специфичные типы драйверов |
| `drivers-common` | `crates/drivers-common` | Трейты и типы драйверов |
| `drivers-aarch64` | `crates/drivers-aarch64` | Реализации драйверов (PL011, GIC) |
| `kernelspace` | `crates/kernelspace` | Основная логика ядра |
| `kobject` | `crates/kobject` | Kernel objects, handles и IPC |
| `scheduler` | `crates/scheduler` | Процессы, потоки и планирование |
| `syscall` | `crates/syscall` | Syscall ABI и dispatch |
| `fdt` | `crates/fdt` | Парсер Flattened Device Tree |
| `io` | `crates/io` | `ByteSink`, `Writer`, форматирование |
| `log` | `crates/log` | Макросы логирования (`klog`) |
| `memory` | `crates/memory` | Frame allocator, bitmap, VA-аллокаторы |
| `test-harness-qemu` | `crates/test-harness-qemu` | Хост-часть QEMU тест-харнесса |
| `test-harness-qemu-aarch64` | `crates/test-harness-qemu-aarch64` | Гостевая часть QEMU тест-харнесса |
| `userspace` | `crates/userspace` | User image loader и user-VM planning |
| `util` | `crates/util` | Утилиты |
| `xtask` | `xtask` | Хост-инструмент сборки (CLI) |
