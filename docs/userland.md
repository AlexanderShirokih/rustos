# Userland

Userland — init radmisk контейнер, который принимает управление от ядра и начинает инициализацию всей userspace-системы.
Userspace-процесс — freestanding-бинарь, который несёт весь свой код и работает
поверх syscall ABI ядра. С ядром он общается через системные вызовы. Код программ userland лежит в `user/`.

## Состав

Userspace опирается на несколько крейтов из `lib/`:

| Крейт                | Роль                                                           |
|----------------------|----------------------------------------------------------------|
| `lib/runtime`        | svc-обёртки syscall ABI; глобальная куча; транспорты IPC; sync |
| `lib/ipc`            | кодек кадров и типы контрактов поверх транспортов              |
| `lib/userland`       | модель образа: сегменты, права, точка входа, валидация         |
| `lib/userland-image` | бинарный формат `userland.img` (encode/decode)                 |
| `lib/bootstrap-abi`  | контракт `Bootstrap` — канал процесса к ядру                   |

## Образ

Все программы userland упакованы в один бинарный образ `userland.img`. Ядро
получает его как blob и из него поднимает первый процесс.

### Структура файла

| Поле        | Смещение | Размер | Описание                       |
|-------------|----------|--------|--------------------------------|
| magic       | 0        | 8 B    | `USRLIMG\0`                    |
| version     | 8        | u16 LE | версия формата (текущая: 1)    |
| entry_count | 10       | u16 LE | число программ в образе (>= 1) |
| total_size  | 12       | u64 LE | полный размер файла в байтах   |

За заголовком (20 B) следуют записи (entry), затем payload-секция.

### Запись (entry)

| Поле           | Смещение | Размер | Описание                           |
|----------------|----------|--------|------------------------------------|
| payload_offset | 0        | u64 LE | смещение payload программы в файле |
| payload_size   | 8        | u64 LE | размер payload в байтах            |
| entry_va       | 16       | u64 LE | точка входа в VA процесса          |
| stack_size     | 24       | u64 LE | размер стека в байтах              |
| segment_count  | 32       | u16 LE | число сегментов                    |
| name_len       | 34       | u16 LE | длина имени (UTF-8, без нуля)      |

За заголовком записи (36 B) идут дескрипторы сегментов, затем имя (UTF-8).

### Сегмент

| Поле        | Смещение | Размер | Описание                                      |
|-------------|----------|--------|-----------------------------------------------|
| file_offset | 0        | u64 LE | смещение данных сегмента в payload            |
| file_size   | 8        | u64 LE | размер данных в файле                         |
| va_base     | 16       | u64 LE | базовый VA в адресном пространстве процесса   |
| mem_size    | 24       | u64 LE | размер в памяти (>= file_size; разница — BSS) |
| flags       | 32       | u32 LE | права: 0=RW, 1=RO, 2=RX                       |

Сегменты и их `va_base` выровнены на страницу (`USERLAND_PAGE_SIZE = 4096`).

Первая программа в образе является bootstrap-процессом. Образ собирается на host:
инструмент читает ELF каждой программы, раскладывает её секции по сегментам с
правами и склеивает записи в `userland.img`. Состав образа задаёт TOML-манифест,
путь к которому передаётся в `--image` (по умолчанию `user/rootkeeper/image.toml`):

```toml
version = 1

[[programs]]
package = "rootkeeper"
```

Признак bootstrap-программы и размер стека фиксируются в разделе
`[package.metadata.userland]` её `Cargo.toml`:

```toml
[package.metadata.userland]
bootstrap = true
stack_size = 65536
```

## Жизненный цикл

После инициализации ядро читает blob `userland.img`, декодирует первую запись (bootstrap-entry), создаёт bootstrap-порт,
корневой `Resource` capability, раскладывает сегменты в новое адресное пространство и стартует первый поток.
Точка входа процесса — `_start`, которой в регистре `x0` приходит raw HandleId
bootstrap-порта (Port):

```rust
#[unsafe(no_mangle)]
pub extern "C" fn _start(root: Handle) -> ! { /* ... */ }
```

Подробности Resource-модели — в [architecture.md](architecture.md) и
[syscalls.md](syscalls.md).

## Контракт Bootstrap

Bootstrap — типизированный канал между bootstrap-процессом и ядром. Контракт определён в `lib/bootstrap-abi`.
Через него корневой процесс запрашивает корневые capability.

## Как написать процесс

Программа userland — бинарь без стандартной библиотеки и без `main`, с
собственными `_start` и `panic_handler`.

```rust
#![no_std]
#![no_main]

use bootstrap::BootstrapClient;
use runtime::{PortTransport, thread_exit};
use syscall::Handle;

#[unsafe(no_mangle)]
pub extern "C" fn _start(root: Handle) -> ! {
    let client = BootstrapClient::new(PortTransport::client(root));
    let result = client.log_str("init", "process started");

    thread_exit(u64::from(result.is_err()))
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop { core::hint::spin_loop() }
}
```

Что нужно новому процессу помимо кода:

- линкер-скрипт `link.ld`: точка входа `_start`, базовый адрес образа
  (`0x00400000`) и выравнивание секций `.text/.rodata/.data/.bss` на страницу
  (4 KiB). Скрипт подключается из `build.rs` через `cargo:rustc-link-arg=-T`;
- раздел `[package.metadata.userland]` нужен, если процесс — кандидат в
  bootstrap (`bootstrap = true`, `stack_size`);
- включение пакета в TOML-манифест образа.

## Сборка и запуск

Команды сборки образа и запуска на QEMU — в [commands.md](commands.md).

## Тесты

Интеграционные тесты ядра выполняются двумя проходами.

Первый проход — kernel-сторона: ядро собирается с фичей `kernel-tests`, тесты выполняются в пространстве ядра.

Второй проход — userspace-сторона: собирается `testrunner` как обычный userland-контейнер (`user/testrunner/image.toml`),
и те же `#[kernel_test]`-функции прогоняются в userspace. `testrunner` устанавливает writer, шлющий лог через
`Bootstrap` в ядро, и exit-делегат.

```rust
#[kernel_test]
fn userland_smoke() {
    let sum: u64 = (1..=10).sum();
    kernel_tests::kassert_eq!(sum, 55);
    kernel_tests::kassert!(sum.is_multiple_of(5));
}
```
