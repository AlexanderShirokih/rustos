# Протокол userland bootstrap: rootkeeper, bootstrap-log

## Обзор

Цепочка загрузки: ядро грузит `userland.img` из initrd -> production-init внутри init-таска
вызывает `kernelspace::bootstrap::spawn_rootkeeper` (parse -> bootstrap_entry -> мост ->
канал -> spawn EL0) -> rootkeeper шлёт RKHELLO и спит до закрытия peer'а ->
kernel-таск `bootstrap-log` читает кадры local-конца и печатает в klog. Тот же
`spawn_rootkeeper` вызывается E2E-тестом, что обеспечивает прогон ровно production-пути.

Ключевые инварианты реализации:

- `sys_object_wait_one` (kernel/syscall/src/bridge.rs:262-270): всегда `Some(timeout_ns)`,
  `0` = poll, пустая маска = InvalidArgument; возврат `i64`: `>=0` -- observed-маска,
  `<0` -- `-(SyscallError)`; `Timeout = 9` => на проводе `-9` (kernel/syscall/src/error.rs:39,59-61).
  Коды задокументированы как стабильный ABI (error.rs:16-17, docs/syscalls.md:68).
- Kernel-сторона `object_wait_one(id, mask, Option<u64>)` (kernel/kobject/src/api.rs:84-91):
  `None` -- бесконечное чисто сигнальное ожидание (таймер не участвует); уже поднятые биты
  проверяются под локом до парковки, observed латчится в waker -- кадр "до" установки
  ожидания потерян быть не может (анти-флак-инвариант).
- Сигналы: `CHANNEL_READABLE 1<<0`, `CHANNEL_PEER_CLOSED 1<<1` (kobject/src/channel.rs:23-30,
  docs/syscalls.md:49-50 -- стабильный ABI); `THREAD_TERMINATED 1<<0` (thread.rs:14),
  `PROCESS_TERMINATED 1<<0` (process.rs:14); `EVENT_SIGNALED` (event.rs).
- `UserProcessLaunchInfo` экспортирует `process_object: Arc<ProcessObject>` и
  `thread_object: Arc<ThreadObject>` (kernel/scheduler/src/user.rs:76-88) -- teardown через
  TERMINATED доступен без прохода по handle-table.
- `Rights::defaults_for(Channel)` включает WRITE|WAIT -- rootkeeper'у ничего добавлять не нужно.
- `kernel/syscall` зависит и от `kobject`, и от `userland-abi` -- guard-тесты зеркал размещаемы
  в `kernel/syscall/src/numbers.rs` (tests-блок уже существует).
- `kernelspace` уже зависит от `process` и `userland-abi` (Cargo.toml) -- мост доступен из init.
- Production-init и kernel-tests взаимоисключены фичей (`pick_init_task`,
  boot_primary.rs:144-154): изменения init.rs не влияют на qemu-suite.

## 1. Жизненный цикл rootkeeper: hello + teardown

**Решение: rootkeeper пишет RKHELLO, затем `object_wait_one(bootstrap_handle,
CHANNEL_SIGNAL_PEER_CLOSED, u64::MAX)` на самом bootstrap-канале -- сигнальный сон до
закрытия kernel-конца.** Никаких новых syscall'ов, handle'ов и периодических кадров.

Семантика возврата:

- `ret >= 0` -- observed-маска; маска из одного бита `CHANNEL_SIGNAL_PEER_CLOSED` (`1<<1`,
  зеркало в userland-abi) => любой неотрицательный возврат означает закрытие kernel-конца:
  `ThreadExit(0)`.
- `ret < 0` -- дефект окружения: `ThreadExit(1)`.

userland не имеет бесконечного wait (мост `sys_object_wait_one` -- всегда `Some(timeout_ns)`);
`u64::MAX` даёт насыщённый дедлайн в ядре (`block_current_until` -- `saturating_add`,
scheduler.rs:817), т.е. практически бесконечный сон без overflow. Маска -- строго PEER_CLOSED
(ядро не пишет rootkeeper'у, READABLE не поднимется; единственная причина пробуждения однозначна).

PEER_CLOSED -- протокол завершения: закрытие local-конца (teardown теста, выход логгера)
сигнально (без таймера) будит rootkeeper из сна, он выходит детерминированно за миллисекунды.

Отвергнуто: периодический heartbeat-кадр (liveness-сигнал избыточен -- ядро владеет
жизненным циклом процесса и наблюдает его через PROCESS_TERMINATED/THREAD_TERMINATED, RKHELLO
уже доказывает живость EL0 и IPC, пропавший beat никто не потребляет); новый sleep-syscall
(расширение стабильного ABI; чистый сон не наблюдает PEER_CLOSED => зомби и медленный teardown);
отдельный Event-тикер (второй initial handle и KO, который некому сигналить).

## 2. Формат кадров и ABI-зеркало

Кадры bootstrap-канала не меняются (владелец -- `abi/userland-abi/src/bootstrap.rs`, реэкспорт
в lib.rs): `BOOTSTRAP_HELLO_MAGIC=*b"RKHELLO\0"`, `BOOTSTRAP_ABI_VERSION=1`,
`BOOTSTRAP_HELLO_SIZE=10`, `parse_bootstrap_hello`; RKLOG-кадры лога (`parse_bootstrap_log`).
Периодический RKBEAT-кадр удалён вместе с heartbeat-механизмом (раздел 1).

ABI-зеркало в `abi/userland-abi/src/syscall.rs` -- одно (только реально потребляемая константа):

```
pub const CHANNEL_SIGNAL_PEER_CLOSED: u32 = 1 << 1;
```

READABLE/WRITABLE и `SYSCALL_RETURN_TIMEOUT` не зеркалируются -- нет userland-потребителя
(rootkeeper трактует любой `ret < 0` как дефект, на код timeout не ветвится). Дрейф исключён
guard-тестом в tests-блоке `kernel/syscall/src/numbers.rs` (крейт видит kobject и userland-abi):
`CHANNEL_SIGNAL_PEER_CLOSED == kobject::CHANNEL_PEER_CLOSED`.

Enum `BootstrapFrame`/`parse_bootstrap_frame` в ABI не вводится (два парсера по
магии достаточно); диспетчеризация -- kernel-side чистой функцией `classify_frame` (см. раздел 3).
`encode_bootstrap_hello` не вводится (преждевременная симметрия; hello строит rootkeeper
вручную как сейчас, kernel-тест логгера собирает 10 байт локально).

## 3. Модуль kernelspace::bootstrap

Новый модуль `kernel/kernelspace/src/bootstrap.rs` (`pub mod` в lib.rs) -- общий seam
для production и тестов. Содержит spawn_rootkeeper и логгер. Зависимости: kobject, scheduler,
klog, userland-abi, process -- все уже в Cargo.toml; никаких arch-терминов (`user_va_end`
приходит параметром `usize`, `A::USER_VA_END` подставляет только generic-вызов).

```
pub struct RootkeeperLaunch {
    pub channel: Arc<Channel>,        // local-конец, остаётся у ядра
    pub info: UserProcessLaunchInfo,  // process/thread Arc'и для наблюдения TERMINATED
}
pub enum RootkeeperSpawnError { Image(UserlandImageError), Bridge(UserImageFromAbiError), Spawn(SpawnUserError) }

/// parse -> bootstrap_entry -> мост -> Channel::create_pair(0) -> spawn с initial_handles[0].
pub fn spawn_rootkeeper(launcher: &dyn UserProcessLauncher, blob: &[u8], user_va_end: usize)
    -> Result<RootkeeperLaunch, RootkeeperSpawnError>;

/// Спавнит kernel-таск "bootstrap-log": кадры local-конца -> klog, выход по PEER_CLOSED.
pub fn spawn_bootstrap_log(scheduler: &Arc<dyn SchedulerService>, local: Arc<Channel>)
    -> Result<(), &'static str>;

/// Тело цикла логгера; вызывается и kernel-тестом (production-API без test-only параметров).
pub fn run_bootstrap_log(local: &Arc<Channel>);

enum BootstrapFrame { Hello(BootstrapHello), Log(&str), Unknown { len: usize } }
fn classify_frame(bytes: &[u8]) -> BootstrapFrame   // чистая, host-тестируемая
```

`spawn_rootkeeper`: `UserlandImage::parse(blob)` -> `bootstrap_entry()` ->
`user_image_parts_from_entry(&entry, user_va_end)` -> `parts.image()` ->
`Channel::create_pair(0)` (rootkeeper пишет единственный RKHELLO-кадр, переполнение исключено) ->
`Handle::new(KObject::Channel(peer), Rights::defaults_for)` ->
`launcher.spawn_user_process_with_launch("rootkeeper", &image, Priority::normal(), 2,
UserProcessLaunch::new().initial_handles(vec![peer_handle]).bootstrap_handle(0))`.

Логгер -- точная калька проверенного `timer_server::run_server` (timer_server.rs:85-128):

1. `install_handle(Handle::new(KObject::Channel(local.clone()), READ|WAIT|INSPECT))`.
2. Цикл `object_wait_one(chan_id, CHANNEL_READABLE | CHANNEL_PEER_CLOSED, None)` -- бесконечный,
   чисто сигнальный (таймер не задействован, CPU не жжётся).
3. Выход: `observed & PEER_CLOSED != 0 && local.peek_signals() & READABLE == 0` (очередь
   дочитана) -> `info!("bootstrap-log: peer closed, exiting")`; также на `channel_read -> PeerClosed`.
4. Дренаж `channel_read(chan_id)` до `ShouldWait`; на кадр `classify_frame`:
   Hello -> `info!("rootkeeper: hello v{}")`; Log -> `info!("{}")` (текст кадра);
   Unknown -> `warn!("bootstrap-log: unknown frame, len={}")`.

Имя таска `"bootstrap-log"`, `Priority::normal()`. Завершение взаимно: смерть rootkeeper'а
дропает peer -> логгер выходит; выход логгера дропает последний Arc local -> rootkeeper
выходит. Логгер stateless (печатает что пришло).

## 4. Инициализация: init.rs

`spawn_init_process<A>` (kernel/kernelspace/src/init.rs):

1. До спавна init-таска собрать только Copy/Send-значения: `launcher: Arc<dyn UserProcessLauncher>
   = Arc::new(SchedulerUserProcessLauncher::new(scheduler.handle(), kernel.address_space_factory()))`
   (образец kernel_tests/mod.rs:96-101), `blob: Option<&'static [u8]> = kernel.userland_blob()`,
   `user_va_end = A::USER_VA_END`, `scheduler_service` через `with_runtime_state`.
2. Вся работа с blob -- внутри работающего init-таска (инвариант kernel_tests/mod.rs:133-137:
   удержание разбора до `scheduler.start()` ловило hang; в closure уезжает только Copy-ссылка).
3. В замыкании: `blob == None` -> `warn!("userland blob missing; rootkeeper not started")`,
   return (ядро живёт на idle -- idle-поток существует, scheduler.rs:480-487);
   иначе `bootstrap::spawn_rootkeeper(launcher.as_ref(), blob, user_va_end)`:
   `Err` -> `warn!`, return; `Ok(launch)` -> `info!` + `spawn_bootstrap_log(&scheduler_service,
   launch.channel)` (`Err` -> `warn!`). Ни одной паники на userland-пути; `expect` остаётся
   только на спавне самого init-таска (неустранимый сбой bootstrap'а ядра, как сейчас).

**Демо-таски удаляются** (`spawn_demo_processes` целиком: timer-server pilot, test-process,
оба demo-принтера, init.rs:48-91). Обоснование: контракт production-init -- запуск
rootkeeper-цепочки; demo-логи каждые 200-700 мс топят вывод rootkeeper'а (подрывают цель
"видимость в klog"); постоянная долбёжка таймера маскирует ровно тот класс багов, который
фиксит w-timer. Удаление -- отдельным коммитом внутри w-kernel; откат дешёвый. Модуль
`timer_server.rs` не трогаем (pub API библиотечного крейта, dead-code warnings не появятся).
В qemu-suite это ничего не меняет: production-init не компилируется в прогон kernel-tests.

## 5. Тесты

Принципы (нулевая терпимость к флакам):

- Ни одного sleep-поллинга. Каждое ожидание -- `object_wait_one` по сигналу; bounded timeout --
  детектор провала, не синхронизация (хэппи-пас будится сигналом за миллисекунды).
- Ассерты на содержимое (RKHELLO version/magic), никогда -- на тайминги.
- Явный teardown каждого E2E: `handle_close(chan_id)` + drop всех Arc local ->
  PEER_CLOSED сигнально будит rootkeeper из сна -> bounded ожидание `THREAD_TERMINATED`
  на `info.thread_object` (без этого EL0-процесс утекает между тестами -- ровно
  leak-state-паттерн текущего hang'а).
- Exit rootkeeper детерминирован: PEER_CLOSED => exit(0). Тесты ассертят факт выхода
  (`THREAD_TERMINATED`), не код.
- kassert-фейл = немедленный exit(1) QEMU (macros.rs:7-13) -- 5-секундные бюджеты не суммируются.
- Оговорка: bounded-timeout как детектор провала сам ходит через таймерный wake-путь --
  до w-timer провальный сценарий может проявляться как hang, а не FAIL; хэппи-пасы
  полностью сигнальные и от таймера не зависят.

### 5.1 Host-тесты

- `userland-abi/src/bootstrap.rs`: hello valid/short/bad-magic/bad-version; log encode/parse
  round-trip и взаимная неперепутываемость магий hello/log.
- `kernel/syscall/src/numbers.rs` (tests): guard-тест ABI-зеркала `CHANNEL_SIGNAL_PEER_CLOSED`.
- `kernelspace/src/bootstrap.rs` (tests): `classify_frame` -- hello/log/unknown, включая
  префикс-коллизии.

### 5.2 Rework `userland_rootkeeper_bootstrap_handshake`

Файл: hal-aarch64/src/kernel_tests/userland_bootstrap.rs.

- Спавн через `kernelspace::bootstrap::spawn_rootkeeper(user_process_launcher().as_ref(),
  blob, Aarch64Context::USER_VA_END)` -- production-путь вместо копии моста/канала.
- `install_handle(local: READ|WAIT|INSPECT)`; `object_wait_one(chan_id,
  READABLE|PEER_CLOSED, Some(WAIT_BUDGET_NS))` (`WAIT_BUDGET_NS = 5e9` нс -- детектор провала,
  не синхронизация); kassert `observed & READABLE != 0`; `channel_read` -> `parse_bootstrap_hello`
  -> ассерты version/magic.
- Teardown: `handle_close(chan_id)` + drop(local) -> `install_handle(KObject::Thread(
  info.thread_object), WAIT|INSPECT)` -> `object_wait_one(THREAD_TERMINATED, Some(WAIT_BUDGET_NS))`.

Хэппи-пас полностью сигнальный (hello пишется до сна rootkeeper'а; wake тест-потока --
signal-path) -- тест зеленеет до фикса таймера и разблокирует сейчас висящий suite.

Покрытие таймера: heartbeat-E2E удалён вместе с протоколом, поэтому ни один userland-E2E
больше не упражняет timer-driven пробуждение спящего EL0-потока (хэппи-пас будится сигналом).
w-timer-трек теряет userland-гейт; таймерный путь остаётся под kernel-тестами scheduler/hal.

### 5.3 Новый kernel-тест `bootstrap_log_exits_on_peer_close`

Файл: kernel/kernelspace/src/kernel_tests/bootstrap_log.rs, mod в kernel_tests/mod.rs.

`create_pair(0)`; `Event::new()`; спавн таска `move || { run_bootstrap_log(&local);
event.signal(EVENT_SIGNALED, 0) }`; `peer.write(hello-байты, собранные локально)`,
`peer.write(мусор)`; `drop(peer)`; handle-based
`object_wait_one(EVENT_SIGNALED, Some(5e9))` (образец event_via_scheduler.rs). Покрывает
дренаж, unknown-ветку и выход по PEER_CLOSED чисто сигнально -- без user-процесса и таймера.
Event-обёртка живёт в тест-замыкании: production-API без test-only параметров.

### 5.4 Регистрация тестов

`userland_rootkeeper_bootstrap_handshake` добавить в `should_run_early`
(lib/test-harness-qemu/src/runner.rs:94-108) -- спавнит полноценный user-процесс/AS,
заявленный критерий раннего батча.

### 5.5 Production-прогон

В acceptance w-kernel -- ручной `timeout 15 cargo xtask build devices/spec/qemu-aarch64.yaml
--run` с проверкой строки `rootkeeper: hello v1` в klog (в production peer не закрывается,
rootkeeper остаётся в сне). Закрывает дыру: production-init не исполняется в qemu-suite.

Бюджет времени qemu-suite: handshake-rework -- с до-5с поллинга до ~миллисекунд;
bootstrap_log -- миллисекунды. Suite остаётся в секундах, `--timeout 20` не увеличивается
(hang лечится поиском дедлока, не таймаутом).

## 6. Зависимости модулей

```
userland-abi  (seam: SyscallOp, формат образа, RKHELLO/RKLOG,
  |            зеркало CHANNEL_SIGNAL_PEER_CLOSED)
  |                       ^                ^                   ^
  |  guard-тесты зеркал   |                |                   |
  +--- kernel/syscall ----+      user/rootkeeper    kernelspace/bootstrap.rs
       (видит kobject и ABI)     (EL0: svc 0x11/0x21/0x52)   (spawn_rootkeeper,
                                                              bootstrap-log, classify_frame)
                                          ^                    ^      ^         ^
                                bootstrap-канал (пара)         |      |         |
                                                       process |  init.rs   hal-aarch64/
                                                     (мост)  --+ (production) kernel_tests/
                                                                              userland_bootstrap.rs
                                                                              (E2E, Aarch64Context::USER_VA_END)
```

Arch-конкретика (`USER_VA_END`) появляется только в generic `spawn_init_process<A>` и в
hal-aarch64-тестах. Workstreams: w-abi -> {w-rootkeeper, w-kernel} параллельно; w-tests
собирает всё (handshake-коммит сдаётся до w-timer); w-timer -- параллельный трек,
эксклюзивный владелец крейта scheduler и hal-таймерного пути; w-verify -- финальная
приёмка. Файловые множества попарно не пересекаются; `kernelspace/src/kernel_tests/mod.rs`
принадлежит w-kernel, `hal-aarch64/src/kernel_tests/mod.rs` -- w-timer (регистрация его
regression-модуля); w-tests правит только уже зарегистрированный userland_bootstrap.rs и
runner.rs.

## 7. Вне scope

Никаких новых syscall'ов (sleep, debug-log), никакого периодического heartbeat, никакого
Event-тикера, никакого frame-enum/encode_hello в ABI, зеркала только реально потребляемых
констант, никакого userland-runtime-крейта, никакой stateful-логики в логгере, не ассертим
exit-код и klog-строки, не растим qemu-таймаут, не трогаем timer_server.rs.

## 8. Риски

- R1 (мёртвый логгер): rootkeeper пишет единственный RKHELLO (результат игнорируется);
  смерть логгера дропает local-конец => PEER_CLOSED будит rootkeeper, он выходит.
- R2 (production-init не исполняется в qemu-suite): цикл логгера покрыт kernel-тестом 5.3,
  мост и spawn-цепочка -- E2E через общий spawn_rootkeeper; плюс ручной --run в acceptance w-kernel.
- R3 (timer-driven пробуждение EL0 без E2E): heartbeat-E2E удалён, userland-гейта у w-timer нет;
  таймерный путь остаётся под kernel-тестами scheduler/hal (осознанная потеря покрытия).
- R4 (до w-timer провал bounded-wait может выглядеть как hang, а не FAIL): принято осознанно;
  хэппи-пасы сигнальные, qemu-runner имеет внешний --timeout 20 как последний рубеж.

## Приложение A. Диагноз зависания userland_rootkeeper_bootstrap_handshake (2026-06-09)

Root cause (верифицировано 3/3 прогонами qemu-test): off-by-one на стыке семантик интервалов.
Interval полуоткрытый [start, end) (lib/collections/src/interval_set.rs), MemoryRange
закрытый [start, end] (kernel/memory/src/memory_range.rs, frame_count = end-start+1). В
MemorySetup::prepare (kernel/hal-aarch64/src/memory/memory_setup.rs:154-156) эксклюзивный
interval.end передавался как инклюзивный конец MemoryRange -- каждый свободный регион получал
один фантомный фрейм ЗА своей границей. Фантомный фрейм региона под ядром = PA 0x40200000,
нулевая страница образа ядра с exception vectors (VBAR=...40200800). Next-fit курсор
аллокатора к 21-му тесту дошёл до хвоста региона и выдал эту страницу под L3-таблицу
user-AS rootkeeper: PageTable::new() занулил вектора. Первый же SVC из EL0 ушёл в
зануленный вектор: бесконечный шторм исключений EC=0 с замаскированным DAIF, без паники
и вывода. "Любой лог маскирует баг", потому что меняет раскладку образа и место попадания
фантомного фрейма, а не тайминги.

Фикс: конвертация эксклюзивного конца в инклюзивный отступом на страницу назад
(aligned_down(end-1)) при построении free_heap_regions_iter. Заодно устраняет фантомный
фрейм за концом RAM и за initrd.

Сопутствующее укрепление (верифицировано отдельно 3/3): global_asm в
kernel/hal-aarch64/src/boot/protocol/linux_arm64/header.rs открывает .section .head и не
восстанавливает секцию; блок векторов в
kernel/hal-aarch64/src/exception/exceptions.rs:52 не задаёт секцию и наследует .head.
Защита страницы векторов держится на случайном делении страницы с _text_start. Фикс: явная
директива .text в global_asm векторов (именно .text, а не .section .text -- последняя
ломает Mach-O host-ассемблер на macOS) + .pushsection/.popsection в header.rs.

Зафиксированные остаточные риски (вне scope текущей работы):

- From<MemoryRegion> for MemoryRange (layout.rs:135-139) -- та же exclusive->inclusive
  несостыковка, живых использований нет.
- reserve_frames_exact в memory_setup.rs передаёт aligned_up эксклюзивный конец как
  инклюзивный фрейм -- перерезервирование на один фрейм (безопасное направление).
- Закрытая семантика MemoryRange [start, end] в целом error-prone, миграция на
  полуоткрытую -- отдельный рефакторинг.
- gicv3 пишет EOI после handler'а -- при context switch из IRQ-handler'а EOI уезжает
  с вытесненным потоком.
