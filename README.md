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

**Arch-слой.** Загрузка, MMU, прерывания, context switch изолированы в `hal-aarch64`
за trait-интерфейсами. Планировщик, kobject, syscall и memory не зависят от
платформенного кода — DAG крейтов это гарантирует на этапе компиляции.

---

## IPC

Channel несёт байты и хендлы одним сообщением. Передача хендла перемещает
capability: у отправителя запись исчезает из handle-таблицы, у получателя
появляется. Права сужаются через `handle_duplicate` перед отправкой. Поверх
канала строятся типизированные IPC-контракты.

```rust
// Передать другому процессу регион памяти с правом только читать
let region_h = memory_create_virtual(4096, AccessMask::R | AccessMask::W)?;
let _va = memory_map(region_h, 4096, UserMemFlags::ReadWrite)?;

let readonly_h = handle_duplicate(
    region_h,
    Rights::WRITE | Rights::READ | Rights::TRANSFER,
    0, // без значка (badge)
)?;

channel_write(peer_h, &[], &[readonly_h])?;
// readonly_h больше нет в handle-таблице текущего процесса

// Закрытие region_h отзывает производный readonly_h у получателя
handle_close(region_h)?;
```

```rust
// RPC с ожиданием ответа
let (left_h, right_h) = channel_create()?;
channel_write(right_h, request, &[reply_event_h])?;

let observed = signal_wait_one(
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
    Rights::READ | Rights::TRANSFER,
    0, // без значка (badge)
)?;
channel_write(supervisor_h, &[], &[wait_h])?;
handle_close(process_h)?;

// На стороне supervisor'а:
let observed = signal_wait_one(received_process_h, PROCESS_TERMINATED, timeout_ns)?;
if observed & PROCESS_TERMINATED != 0 {
    let exit_code = process_exit_code(received_process_h)?;
}
```

Полный ABI с таблицей прав, сигналов и примерами — [docs/syscalls.md](docs/syscalls.md);
формат типизированных IPC-контрактов — [docs/ipc.md](docs/ipc.md).

---

## Userland

Программы userland — freestanding `no_std`-бинари, упакованные в один образ
`userland.img`. Ядро поднимает из образа первый процесс и передаёт ему
bootstrap-хендл; дальше процессы общаются с ядром и между собой через каналы и
типизированные контракты. Состав userland, рантайм syscall-обёрток, формат
образа и жизненный цикл — [docs/userland.md](docs/userland.md).

---

## Архитектура крейтов

Крейты сгруппированы по доменам каталогов. Зависимости направлены вниз: платформа
и оркестрация -> механизмы ядра -> общие библиотеки.

```
lib/   — общие библиотеки и ABI
  syscall                       — syscall ABI: номера операций, ошибки, сигналы
  ipc, ipc-macros, ipc-schema   — типизированные IPC-контракты поверх канала
  bootstrap-abi                 — контракт Bootstrap: канал процесса к ядру
  runtime                       — userland-обёртки syscall и транспорт IPC
  userland, userland-image      — модель и бинарный формат образа userland.img
  collections, io, util         — no_std-библиотеки общего назначения
  test-harness-macros / -qemu   — integration-тесты поверх QEMU exit device

kernel/ — механизмы, драйверы, оркестрация, платформа
  memory                        — физическая и виртуальная память, адресные пространства
  kobject                       — handle-таблица, сигналы, каналы, объекты процессов/потоков
  scheduler                     — потоки, приоритеты, очереди готовности, context switch
  process                       — загрузка user-образа в адресное пространство
  syscall-kernel                — диспетчер syscall и граница user/kernel
  fdt, klog                     — разбор FDT, kernel-лог
  drivers-common (+ -aarch64)   — контракты драйверов и обнаружение устройств
  drivers-aarch64               — UART, таймер, контроллер прерываний (GIC)
  kernelspace                   — порядок инициализации, runtime-мосты, запуск userland
  hal-common                    — boot-протокол, BootInfo, аппаратное описание
  hal-aarch64 (+ -asid, -paging)— boot, MMU, CPU-контекст; итоговый kernel-бинарь

user/  — userland-программы
  rootkeeper                    — bootstrap-процесс
  testrunner                    — прогон тестов в userspace

tools/ — host-инструменты
  userland-image                — сборка userland.img из ELF
```

Полный инвентарь крейтов и правила слоёв — [docs/overview.md](docs/overview.md) и
[docs/architecture.md](docs/architecture.md).

---

## Syscall ABI

16-битный номер операции, до шести `u64`-аргументов, `i64` возврат
(отрицательный — `-(SyscallError as u32)`). `HandleId` — ненулевой `u32`.

| Диапазон    | Подсистема                                                  |
|-------------|-------------------------------------------------------------|
| `0x10–0x12` | Object — сигналы, ожидание одного и нескольких объектов     |
| `0x20–0x22` | Channel — создание, запись, чтение                          |
| `0x30–0x31` | Handle — закрытие, дублирование                             |
| `0x40–0x45` | Process — создание, self, load image, exit code, terminate, start |
| `0x50–0x54` | Thread — создание, self, exit, exit code, terminate         |
| `0x60–0x67` | Memory — VMO, MMIO, map, remap, allocate, free, inspect     |
| `0x70–0x74` | Mailbox — создание, queue, синхронное и async-ожидание, cancel |

---

## Быстрый старт

```bash
rustup component add llvm-tools-preview
cargo install cargo-binutils
# qemu-system-aarch64 — через пакетный менеджер

cargo xtask build devices/spec/qemu-aarch64.yaml --run
cargo xtask qemu-test --timeout 20
cargo test --workspace
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
  format: android_boot_v1   # linux_arm64 | android_boot_v1 | android_boot_v2
  offset: 0x40080000        # адрес загрузки ядра (KERNEL_OFFSET)
  base: 0x40000000          # база boot image (только android_boot_*)
  dtb: /devices/dtb/my-device.dtb  # DTB (только android_boot_*)

run:
  - "fastboot boot target/build/boot.img"

debug:
  - "fastboot boot target/build/boot.img"
```

**Форматы образов:**

| `format`          | Артефакт                                                        | Применение                                                                      |
|-------------------|-----------------------------------------------------------------|---------------------------------------------------------------------------------|
| `linux_arm64`     | `target/build/kernel.bin` + sidecar `target/build/userland.img` | QEMU, Raspberry Pi, bare-metal; sidecar должен быть передан как initrd          |
| `android_boot_v1` | `target/build/boot.img`                                         | Android-устройства, header v1, appended DTB, `userland.img` упакован в ramdisk  |
| `android_boot_v2` | `target/build/boot.img`                                         | Android-устройства, header v2, отдельный DTB, `userland.img` упакован в ramdisk |

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

Работающие конфиги: Xiaomi Redmi Note 7 (`android_boot_v1`), Raspberry Pi 5 (`linux_arm64`).
PR с новыми устройствами приветствуются.

---

## Что реализовано

- Capability-based handle-таблица с передачей и сужением прав
- Отзыв полномочий: каскадное закрытие хендла отзывает поддерево производных
- Двусторонний channel IPC с передачей capability
- Типизированные IPC-контракты поверх канала
- Mailbox с синхронным и async-ожиданием
- Userland: образ программ `userland.img`, рантайм syscall-обёрток, bootstrap-протокол
- Разделяемая память: VMO и MMIO как kernel-объекты (создание, map, remap, инспекция, передача)
- Process и thread lifecycle: создание, завершение, terminate, ожидание сигналов
- `signal_wait_one`/`signal_wait_many` с сигналами и таймаутом
- QEMU integration tests (kernel- и userland-проход)
- Загрузка на Xiaomi Redmi Note 7 и Raspberry Pi 5

Не реализовано:

- UEFI boot
- Драйверы периферии (GPIO, I2C, SPI)
- Userspace-сервисный слой (драйверы, ФС, сеть)

---

## Документация

- [docs/overview.md](docs/overview.md) — обзор проекта и инвентарь крейтов
- [docs/architecture.md](docs/architecture.md) — структура ядра, границы подсистем, boot-протокол
- [docs/syscalls.md](docs/syscalls.md) — полный syscall-ABI: права, сигналы, ошибки, примеры
- [docs/ipc.md](docs/ipc.md) — формат типизированных IPC-контрактов
- [docs/userland.md](docs/userland.md) — устройство userland
- [docs/environment.md](docs/environment.md) — toolchain, зависимости, device specs
- [docs/commands.md](docs/commands.md) — команды сборки и тестирования
- [docs/code-style.md](docs/code-style.md) — соглашения по стилю кода и комментариям
- [Статья на Хабре](https://habr.com/ru/articles/962680/) — обзор проекта

---

## Лицензия

Экспериментальный проект. Распространяется как есть.
