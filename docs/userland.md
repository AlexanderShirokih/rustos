# Userland

Userland — это всё, что работает вне ядра: драйверы, сервисы и прикладные
программы, каждая в своём адресном пространстве и со своим набором прав. Ядро
даёт только примитивы (потоки, память, каналы, таймеры, capability-хэндлы), а
поведение системы складывается из userspace-процессов, которые этими примитивами
пользуются и общаются друг с другом и с ядром через каналы.

Процесс userland — freestanding-бинарь под `no_std`: он несёт в себе весь свой
код и работает поверх голого ABI ядра. С ядром он общается через syscall, а
ресурсами владеет ровно теми, чьи хэндлы получил при старте или принял по каналу.

Программы userland лежат в каталоге `user/`.

## Состав

Userspace опирается на несколько крейтов из `lib/`:

| Крейт                | Роль                                                          |
|----------------------|---------------------------------------------------------------|
| `lib/runtime`        | тонкие svc-обёртки syscall-ABI; глобальная куча; транспорт IPC |
| `lib/syscall`        | номера операций, коды ошибок, маски сигналов KObject          |
| `lib/ipc`            | кодек кадров и типы контрактов поверх канала                  |
| `lib/userland`       | модель образа: сегменты, права, точка входа, валидация        |
| `lib/userland-image` | бинарный формат `userland.img` (encode/decode)                |
| `lib/bootstrap-abi`  | контракт `Bootstrap` — канал процесса к ядру                  |

Прикладной код процесса зависит от `runtime`, `syscall`, `ipc` и нужных
контрактов; форматом образа и его сборкой занимается host-инструментарий.

## Lib/Runtime

`runtime` — поверхность syscall для userland: по одной функции на каждую операцию
ядра. Процессу доступны вызовы для каналов (`channel_create/read/write`), хэндлов
(`handle_close`), процессов и потоков (`process_*`, `thread_*`), памяти
(`memory_*`) и mailbox (`mailbox_*`). Возврат знаковый: отрицательное значение —
код ошибки. Полный перечень операций и их семантика — в [syscalls.md](syscalls.md).

Глобальный аллокатор процесса — куча поверх `memory_allocate`/`memory_free`:
`alloc` (`Box`, `Vec`, `String`) доступен всегда. Куча безопасна для
многопоточного процесса; крупные аллокации обслуживаются ядром напрямую и
возвращаются ему на освобождении.

Контракты IPC процесс держит поверх канала через `ChannelTransport`; их
устройство описано в [ipc.md](ipc.md).

## Образ

Все программы userland упакованы в один бинарный образ `userland.img`. Ядро
получает его как blob и из него поднимает первый процесс. Образ самодостаточный:
заголовок, метаданные записей и payload с байтами сегментов, все целые
little-endian.

Каждая запись (entry) — одна программа: имя, точка входа (`entry_va`), размер
стека и список сегментов. Сегмент несёт адрес в адресном пространстве процесса
(`va_base`), размер в файле и в памяти (`mem_size >= file_size`, разница — `.bss`)
и права: `RW`, `RO` или `RX`. Сегменты выровнены на страницу
(`USERLAND_PAGE_SIZE = 4096`), что обеспечивается линкер-скриптом программы.

Образ собирается на host: инструмент читает ELF каждой программы, раскладывает её
секции по сегментам с правами и склеивает записи в `userland.img`. Состав образа
задаёт TOML-композиция в `user/images/`:

```toml
version = 1

[[programs]]
package = "rootkeeper"
```

Первый процесс образа — bootstrap-процесс; его выбор фиксируется в манифесте
пакета программы (`[package.metadata.userland] bootstrap = true`) вместе с
размером стека.

## Жизненный цикл

Ядро после инициализации запускает высокоприоритетный init-таск. Init читает
blob `userland.img`, создаёт пару концов канала, отдаёт один конец будущему
процессу как стартовый хэндл, декодирует первую запись образа, раскладывает её
сегменты в новое адресное пространство и стартует первый поток на `entry_va`.

Точка входа процесса — `_start`, которой в `x0` приходит сырой `HandleId`
bootstrap-канала:

```rust
pub extern "C" fn _start(bootstrap_handle: usize) -> ! { /* ... */ }
```

Второй конец канала остаётся у ядра: init дренирует его в цикле, принимая кадры
контракта `Bootstrap` (например `log`) и подмешивая их в kernel-лог. Цикл
крутится, пока процесс жив. Когда процесс завершается, его конец канала
закрывается — ядро видит сигнал `CHANNEL_PEER_CLOSED`, читает exit-код процесса
и выключает машину этим кодом (`system_off(exit_code)`).

Так смерть bootstrap-процесса — это штатное завершение всей системы, а его
exit-код становится кодом выхода машины. Это используют тесты: процесс
отрабатывает, выходит с кодом, машина гаснет, host-обвязка читает код как
вердикт.

## Как написать процесс

Программа userland — `no_std`/`no_main` бинарь с собственным `_start` и
`panic_handler`. `rootkeeper` — минимальный полный пример: залогировать строку
через bootstrap-канал и подождать его закрытия.

```rust
#![no_std]
#![no_main]

use bootstrap::{BootstrapClient, LOG_MESSAGE_MAX};
use ipc::wire::Str;
use runtime::{ChannelTransport, object_wait_one, thread_exit};
use syscall::CHANNEL_PEER_CLOSED;

#[unsafe(no_mangle)]
pub extern "C" fn _start(bootstrap_handle: usize) -> ! {
    let client = BootstrapClient::new(ChannelTransport::new(bootstrap_handle));
    let _ = client.log(Str::<LOG_MESSAGE_MAX>::new("rootkeeper started").unwrap());

    let wait_ret = object_wait_one(bootstrap_handle, CHANNEL_PEER_CLOSED, u64::MAX);
    thread_exit(u64::from(wait_ret < 0))
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop { core::hint::spin_loop() }
}
```

Что нужно новому процессу помимо кода:

- линкер-скрипт `link.ld`: точка входа `_start`, базовый адрес образа и
  выравнивание секций `.text/.rodata/.data/.bss` на страницу (4 КиБ). Скрипт
  подключается из `build.rs` через `rustc-link-arg=-T`;
- `Cargo.toml` с `forced-target = "aarch64-unknown-none"` (через
  `cargo-features = ["per-package-target"]`): бинарь всегда собирается под
  bare-metal без `--target`, поэтому код не гейтится под host-сборку. Раздел
  `[package.metadata.userland]` нужен, если процесс — кандидат в bootstrap
  (`bootstrap = true`, `stack_size`);
- зависимости `runtime`, `syscall`, `ipc` и нужные контракты;
- включение пакета в TOML-композицию образа (`user/images/<имя>.toml`).

Вывод и обмен с другими процессами идут через каналы и контракты: процесс держит
`*Client`/`*Service` контракта поверх `ChannelTransport`. Стартовый
bootstrap-канал — частный случай: на нём объявлен контракт `Bootstrap`, и его
конец процесс получает в `x0`.

## Сборка и запуск

Образ собирается отдельно от ядра:

```bash
cargo xtask build-userland                 # user/images/default.toml -> userland.img
cargo xtask build-userland --image test    # другая композиция
```

Полная сборка устройства включает userland-образ как первый шаг:

```bash
cargo xtask build devices/spec/qemu-aarch64.yaml
```

Запуск на QEMU с прогоном integration-тестов:

```bash
cargo xtask qemu-test --timeout 20
```

Команды и артефакты сборки под разные `boot.format` — в
[commands.md](commands.md).

## Тесты

Тесты ядра идут двумя проходами над одним и тем же production-ядром.

Первый проход — kernel-сторона: ядро собирается с фичей `kernel-tests`, тесты
выполняются в kernel-контексте.

Второй проход — userland-сторона: в образ кладётся `testrunner` как
bootstrap-процесс (`user/images/test.toml`), и те же `#[kernel_test]`-функции
прогоняются уже в userspace против обычного ядра. `testrunner` подключает к
харнессу writer, который шлёт строки лога кадрами контракта `Bootstrap` в ядро, а
по завершении выходит с кодом-вердиктом — машина гаснет этим кодом.

```rust
#[kernel_test]
fn userland_smoke() {
    let sum: u64 = (1..=10).sum();
    kernel_tests::kassert_eq!(sum, 55);
}
```

Второй проход проверяет полный путь через реальный syscall-ABI и загрузку образа.
Критерий, какой тест где живёт, и детали обоих проходов — в
[commands.md](commands.md).
