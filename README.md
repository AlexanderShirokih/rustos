# RustOS Mobile

Экспериментальное capability-based микроядро для AArch64 на Rust.

Проект написан с нуля: загрузчик, планировщик, IPC, управление памятью и
capability-система без заимствованных компонентов. Протестировано на
Xiaomi Redmi Note 7 (Qualcomm SDM660), Raspberry Pi 5 и QEMU.

---

## Устройство ядра

Ядро предоставляет адресные пространства, потоки, планирование, IPC, таймеры
и capability-хендлы. Драйверы, файловые системы, сетевые стеки — userspace.

**Capability-модель доступа.** Доступ к любому ресурсу — через хендл: токен на
kernel-объект с набором прав. Дублировать хендл можно только с сужением прав.
Неявной глобальной власти нет — поток может обращаться только к тому, хендл на
что у него есть. Хендлы передаются через channel: capability переходит из таблицы
отправителя в таблицу получателя.

**Типизация на уровне компилятора.** `#![no_std]`, Edition 2024, nightly. Публичные
API принимают `HandleId`, `PhysicalAddress`, `ProcessId`, `Frame` вместо голых
`usize`. Выравнивания и ёмкости — const generics. Закрытые enum'ы обеспечивают
исчерпывающий `match` по типам объектов и кодам ошибок на этапе компиляции.

**Arch-слой.** Загрузка, MMU, прерывания, context switch изолированы в `hal-aarch64`
за trait-интерфейсами. Планировщик, kobject, syscall и memory не зависят от
платформенного кода — DAG крейтов это гарантирует на этапе компиляции.

---

## IPC

Channel несёт байты и хендлы одним сообщением. Передача хендла перемещает
capability: у отправителя запись исчезает из handle-таблицы, у получателя
появляется. Права сужаются через `handle_duplicate` перед отправкой.

```rust
// Передать другому процессу регион памяти с правом только читать
let region_h = memory_create_virtual(auth_h, 4096, AccessMask::R | AccessMask::W)?;
let _va = memory_map(region_h, 4096, UserMemFlags::ReadWrite)?;

let readonly_h = handle_duplicate(
    region_h,
    Rights::MAP | Rights::READ | Rights::INSPECT | Rights::TRANSFER,
)?;

channel_write(peer_h, &[], &[readonly_h])?;
// readonly_h больше нет в handle-таблице текущего процесса
```

```rust
// RPC с ожиданием ответа
let (left_h, right_h) = channel_create()?;
channel_write(right_h, request, &[reply_event_h])?;

let observed = object_wait_one(
    left_h,
    CHANNEL_READABLE | CHANNEL_PEER_CLOSED,
    timeout_ns,
)?;
if observed & CHANNEL_READABLE != 0 {
    let (bytes_len, handles_count) = channel_read(left_h, &mut response, &mut handles)?;
}
```

```rust
// Передать supervisor'у право ожидать завершения текущего процесса
let process_h = process_self()?;
let wait_h = handle_duplicate(
    process_h,
    Rights::WAIT | Rights::INSPECT | Rights::TRANSFER,
)?;
channel_write(supervisor_h, &[], &[wait_h])?;
handle_close(process_h)?;

// На стороне supervisor'а:
let observed = object_wait_one(received_process_h, PROCESS_TERMINATED, timeout_ns)?;
if observed & PROCESS_TERMINATED != 0 {
    let exit_code = process_exit_code(received_process_h)?;
}
```

Полный ABI с таблицей прав, сигналов и примерами: [docs/syscalls.md](docs/syscalls.md).

---

## Архитектура крейтов

```
kernelspace          — оркестрация: драйверы, scheduler bridge, user init
  ├── scheduler      — потоки, приоритеты, адресные пространства, context switch
  ├── kobject        — handle-таблица, refcount, сигнальные маски
  ├── syscall        — диспетчер и ABI
  ├── memory         — kernel heap, UserVmAllocator, трейт MemoryMapper
  └── userspace      — загрузка user-образа

hal-aarch64          — boot, MMU (4-уровневые таблицы, ASID), GIC, ArchContext
hal-common           — boot-протокол, BootInfo, HwDescription

drivers-aarch64      — UART, таймер
drivers-common       — платформо-независимые интерфейсы

collections, fdt, io, klog, util   — no_std-библиотеки
test-harness-qemu    — integration-тесты поверх QEMU exit device
```

---

## Syscall ABI

16-битный номер операции, до шести `u64`-аргументов, `i64` возврат
(отрицательный — `-(SyscallError as u32)`). `HandleId` — ненулевой `u32`.

| Диапазон | Подсистема |
|---|---|
| `0x10–0x11` | Object — сигналы, ожидание |
| `0x20–0x22` | Channel — создание, запись, чтение |
| `0x30–0x31` | Handle — закрытие, дублирование |
| `0x40–0x44` | Process — создание, self, exit code, terminate |
| `0x50–0x54` | Thread — создание, self, exit, exit code, terminate |
| `0x60–0x67` | Memory — VMO, MMIO, map, remap, allocate, free, inspect |

---

## Быстрый старт

```bash
rustup component add llvm-tools-preview
cargo install cargo-binutils
# qemu-system-aarch64 — через пакетный менеджер

cargo xtask build devices/spec/qemu-aarch64.yaml --run
cargo xtask qemu-test --timeout 60
cargo test --workspace --exclude drivers-aarch64 --exclude hal-aarch64
```

Зависимости и сборка под реальные устройства: [BUILD.md](BUILD.md).

---

## Запустить на своём устройстве

Если у вас есть AArch64-телефон или одноплатник, можно попробовать запустить
ядро на нём. Достаточно описать устройство в YAML-спеке и положить в `devices/spec/`.

```yaml
device:
  name: My Device
  arch: aarch64

boot:
  format: android_boot_v1   # binary | android_boot_v1 | android_boot_v2
  offset: 0x40080000        # адрес загрузки ядра (KERNEL_OFFSET)
  base: 0x40000000          # база boot image (только android_boot_*)
  dtb: /devices/dtb/my-device.dtb  # DTB (только android_boot_*)

run:
  - "fastboot boot target/build/boot.img"

debug:
  - "fastboot boot target/build/boot.img"
```

**Форматы образов:**

| `format` | Артефакт | Применение |
|---|---|---|
| `binary` | `target/build/kernel.bin` | QEMU, Raspberry Pi, bare-metal |
| `android_boot_v1` | `target/build/boot.img` | Android-устройства, header v1, appended DTB |
| `android_boot_v2` | `target/build/boot.img` | Android-устройства, header v2, отдельный DTB |

**Для Android-устройства потребуется:**

1. **DTB** — из stock прошивки (`abootimg -x boot.img`, затем извлечь из appended blob)
   или из дерева ядра производителя.
2. **`offset`** — адрес загрузки ядра. Смотрите `BOARD_KERNEL_BASE` в `BoardConfig.mk`;
   обычно `BOARD_KERNEL_BASE + 0x00008000`.
3. **Разблокированный bootloader** — `fastboot boot` не перезаписывает флеш,
   загрузка одноразовая.

```bash
cargo xtask build devices/spec/my-device.yaml --run
```

Работающие конфиги: Xiaomi Redmi Note 7 (`android_boot_v1`), Raspberry Pi 5 (`binary`).
PR с новыми устройствами приветствуются.

---

## Что реализовано

- Capability-based handle-таблица с передачей и сужением прав
- Двусторонний channel IPC с передачей capability
- Разделяемая память: VMO и MMIO как kernel-объекты (создание, map, remap, инспекция, передача)
- Process и thread lifecycle: создание, завершение, terminate, ожидание сигналов
- `object_wait_one` с сигналами и таймаутом
- QEMU integration tests
- Загрузка на Xiaomi Redmi Note 7 и Raspberry Pi 5

Не реализовано:

- UEFI boot
- Драйверы периферии (GPIO, I2C, SPI)
- Userspace-сервисный слой

---

## Документация

- [docs/architecture.md](docs/architecture.md) — DAG крейтов, адресные пространства, boot-протокол
- [docs/syscalls.md](docs/syscalls.md) — полный ABI: права, сигналы, ошибки, примеры
- [docs/overview.md](docs/overview.md) — инвентарь крейтов
- [docs/commands.md](docs/commands.md) — команды сборки и тестирования
- [Статья на Хабре](https://habr.com/ru/articles/962680/) — обзор проекта

---

## Лицензия

Экспериментальный проект. Распространяется как есть.
