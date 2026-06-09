# Итоговый дизайн: production-цепочка userland (rootkeeper + heartbeat + bootstrap-log)

База — кандидат "testfirst" (победитель обоих судей), с вшитыми cross-ideas: общий
production-модуль `kernelspace::bootstrap` (от "seams"), экспортируемая константа периода
heartbeat (от "minimal"), этапная сдача тестов (от "minimal"). Все must_fix судей закрыты
(см. разделы); фактура перепроверена по коду 2026-06-09.

## 0. Обзор

Цепочка: ядро грузит `userland.img` из initrd -> production-init внутри init-таска
вызывает `kernelspace::bootstrap::spawn_rootkeeper` (parse -> bootstrap_entry -> мост ->
канал -> spawn EL0) -> rootkeeper шлёт RKHELLO, затем раз в секунду heartbeat-кадр RKBEAT ->
kernel-таск `bootstrap-log` читает кадры local-конца и печатает в klog. Тот же
`spawn_rootkeeper` вызывается E2E-тестом — тест прогоняет ровно production-путь.
Фикс таймер-бага — параллельный трек (w-timer), гейтит только heartbeat-E2E.

Проверенная фактура (ключевое):
- `sys_object_wait_one` (crates/syscall/src/bridge.rs:262-270): всегда `Some(timeout_ns)`,
  `0` = poll, пустая маска = InvalidArgument; возврат `i64`: `>=0` — observed-маска,
  `<0` — `-(SyscallError)`; `Timeout = 9` => на проводе `-9` (crates/syscall/src/error.rs:39,59-61).
  Коды задокументированы как стабильный ABI (error.rs:16-17, docs/syscalls.md:68).
- Kernel-сторона `object_wait_one(id, mask, Option<u64>)` (crates/kobject/src/api.rs:84-91):
  `None` — бесконечное чисто сигнальное ожидание (таймер не участвует); уже поднятые биты
  проверяются под локом до парковки, observed латчится в waker — кадр "до" установки
  ожидания потерян быть не может (анти-флак-инвариант).
- Сигналы: `CHANNEL_READABLE 1<<0`, `CHANNEL_PEER_CLOSED 1<<1` (kobject/src/channel.rs:23-30,
  docs/syscalls.md:49-50 — стабильный ABI); `THREAD_TERMINATED 1<<0` (thread.rs:14),
  `PROCESS_TERMINATED 1<<0` (process.rs:14); `EVENT_SIGNALED` (event.rs).
- `UserProcessLaunchInfo` экспортирует `process_object: Arc<ProcessObject>` и
  `thread_object: Arc<ThreadObject>` (crates/scheduler/src/user.rs:76-88) — teardown через
  TERMINATED доступен без прохода по handle-table.
- `Rights::defaults_for(Channel)` включает WRITE|WAIT — rootkeeper'у ничего добавлять не нужно.
- `crates/syscall` зависит и от `kobject`, и от `userland-abi` — guard-тесты зеркал размещаемы
  в `crates/syscall/src/numbers.rs` (tests-блок уже существует).
- `kernelspace` уже зависит от `userspace` и `userland-abi` (Cargo.toml) — мост доступен из init.
- Production-init и kernel-tests взаимоисключены фичей (`pick_init_task`,
  boot_primary.rs:144-154): изменения init.rs не влияют на qemu-suite.

## 1. Q1: механизм секундного сна rootkeeper

**Решение: `ObjectWaitOne(bootstrap_handle, CHANNEL_SIGNAL_PEER_CLOSED, BOOTSTRAP_HEARTBEAT_PERIOD_NS)`
на самом bootstrap-канале.** Никаких новых syscall'ов и handle'ов.

Точная семантика различения (закрывает must_fix "источник -9 и бита PEER_CLOSED"):
- `ret == SYSCALL_RETURN_TIMEOUT` (`-9`, зеркало в userland-abi, guard-тест в crates/syscall) —
  период истёк, peer жив: послать heartbeat, `seq = seq.wrapping_add(1)`, снова в wait.
- `ret >= 0` — observed-маска; при маске из одного бита `CHANNEL_SIGNAL_PEER_CLOSED` (`1<<1`,
  зеркало в userland-abi) любой неотрицательный возврат означает закрытие kernel-конца:
  `ThreadExit(0)`.
- прочие `ret < 0` — дефект окружения: `ThreadExit(1)`.
- Ошибка `ChannelWrite` (peer закрылся в окне Timeout->write, либо очередь полна при мёртвом
  логгере) — `ThreadExit(0)`.

Защитный инвариант (от "minimal", закрывает must_fix о горячем цикле): точное сравнение с
`SYSCALL_RETURN_TIMEOUT` исключает интерпретацию любой иной ошибки как тика; все не-Timeout
пути и ошибки записи ведут к немедленному выходу — горячий цикл невозможен ни в одном
ошибочном режиме. Маска — строго PEER_CLOSED (ядро не пишет rootkeeper'у, READABLE не
поднимется; каждая причина пробуждения однозначна).

Бонус: PEER_CLOSED даром даёт протокол завершения — закрытие local-конца (teardown теста,
выход логгера) сигнально (без таймера) будит rootkeeper из секундного сна, он выходит
детерминированно.

Отвергнуто: новый sleep-syscall (расширение стабильного ABI; чистый сон не наблюдает
PEER_CLOSED => зомби и медленный teardown), отдельный Event-тикер (второй initial handle и
KO, который некому сигналить), поллинг timeout=0 (выжигает CPU).

## 2. Q2: протокол heartbeat

Кадры bootstrap-канала. Владелец — `crates/userland-abi/src/bootstrap.rs` (seam; реэкспорт в lib.rs):

```
// Существующее (НЕ меняется): BOOTSTRAP_HELLO_MAGIC=*b"RKHELLO\0", BOOTSTRAP_ABI_VERSION=1,
// BOOTSTRAP_HELLO_SIZE=10, parse_bootstrap_hello. BOOTSTRAP_ABI_VERSION остаётся 1.

pub const BOOTSTRAP_HEARTBEAT_MAGIC: [u8; 8] = *b"RKBEAT\0\0";
pub const BOOTSTRAP_HEARTBEAT_SIZE: usize = 16;              // magic(8) + seq u64 LE(8)
pub const BOOTSTRAP_HEARTBEAT_PERIOD_NS: u64 = 1_000_000_000; // контракт: кадр раз в период
pub struct BootstrapHeartbeat { pub seq: u64 }
pub enum BootstrapHeartbeatError { PayloadTooShort { needed, actual }, InvalidMagic([u8; 8]) }
pub fn parse_bootstrap_heartbeat(&[u8]) -> Result<BootstrapHeartbeat, BootstrapHeartbeatError>
pub fn encode_bootstrap_heartbeat(seq: u64) -> [u8; BOOTSTRAP_HEARTBEAT_SIZE]  // без аллокаций
```

`seq` стартует с 0, шаг 1 (wrapping) — FIFO канала + единственный писатель дают тестам точные
ассерты `0`, затем `1`. `BOOTSTRAP_HEARTBEAT_PERIOD_NS` — константа протокола (cross-idea от
"minimal"): rootkeeper берёт из неё timeout, тесты выводят бюджеты ожидания (5x период) —
single-source вместо magic-литералов.

ABI-зеркала в `crates/userland-abi/src/syscall.rs` — РОВНО два (must_fix "урезать зеркала"):

```
pub const CHANNEL_SIGNAL_PEER_CLOSED: u32 = 1 << 1;
pub const SYSCALL_RETURN_TIMEOUT: i64 = -9;
```

READABLE/WRITABLE не зеркалируются — нет userland-потребителя. Дрейф исключён guard-тестами
в tests-блоке `crates/syscall/src/numbers.rs` (крейт видит kobject и userland-abi):
`CHANNEL_SIGNAL_PEER_CLOSED == kobject::CHANNEL_PEER_CLOSED`,
`SYSCALL_RETURN_TIMEOUT == SyscallError::Timeout.as_return_value()`.

Enum `BootstrapFrame`/`parse_bootstrap_frame` в ABI НЕ вводится (оба судьи: два парсера по
магии достаточно); диспетчеризация — kernel-side чистой функцией `classify_frame` (см. Q3).
`encode_bootstrap_hello` НЕ вводится (преждевременная симметрия; hello строит rootkeeper
вручную как сейчас, kernel-тест логгера собирает 10 байт локально).

Маркировка временности — БЕЗ комментариев: doc-комментарии описывают только контракт
("Heartbeat-кадр bootstrap-канала: магия + LE-счётчик"). Временность выражена структурно:
heartbeat изолирован в трёх точках (ABI bootstrap.rs, kernelspace/bootstrap.rs, цикл
rootkeeper), зону ответственности задаёт имя таска `bootstrap-log`; фиксация — этот док.
Выпиливание = удаление символов из трёх точек, без археологии.

## 3. Q3: kernel-side — модуль kernelspace::bootstrap (общий seam production и тестов)

Новый модуль `crates/kernelspace/src/bootstrap.rs` (`pub mod` в lib.rs) — слияние
spawn_rootkeeper от "seams" (must_fix обоих судей: E2E гоняет ровно production-путь) и
логгера от "testfirst". Зависимости: kobject, scheduler, klog, userland-abi, userspace —
все уже в Cargo.toml; никаких arch-терминов (`user_va_end` приходит параметром `usize`,
`A::USER_VA_END` подставляет только generic-вызов).

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

enum BootstrapFrame { Hello(BootstrapHello), Heartbeat(BootstrapHeartbeat), Unknown { len: usize } }
fn classify_frame(bytes: &[u8]) -> BootstrapFrame   // чистая, host-тестируемая
```

`spawn_rootkeeper`: `UserlandImage::parse(blob)` -> `bootstrap_entry()` ->
`user_image_parts_from_entry(&entry, user_va_end)` -> `parts.image()` ->
`Channel::create_pair(0)` (capacity 16: при 1 Гц и живом читателе переполнение исключено;
при мёртвом — write упадёт и rootkeeper выйдет) ->
`Handle::new(KObject::Channel(peer), Rights::defaults_for)` ->
`launcher.spawn_user_process_with_launch("rootkeeper", &image, Priority::normal(), 2,
UserProcessLaunch::new().initial_handles(vec![peer_handle]).bootstrap_handle(0))`.
Образец проверен рабочим тестом userland_bootstrap.rs:35-51.

Логгер — точная калька проверенного `timer_server::run_server` (timer_server.rs:85-128):
1. `install_handle(Handle::new(KObject::Channel(local.clone()), READ|WAIT|INSPECT))`.
2. Цикл `object_wait_one(chan_id, CHANNEL_READABLE | CHANNEL_PEER_CLOSED, None)` — бесконечный,
   чисто сигнальный (таймер не задействован, CPU не жжётся).
3. Выход: `observed & PEER_CLOSED != 0 && local.peek_signals() & READABLE == 0` (очередь
   дочитана) -> `info!("bootstrap-log: peer closed, exiting")`; также на `channel_read -> PeerClosed`.
4. Дренаж `channel_read(chan_id)` до `ShouldWait`; на кадр `classify_frame`:
   Hello -> `info!("rootkeeper: hello v{}")`; Heartbeat -> `info!("rootkeeper: heartbeat #{}")`;
   Unknown -> `warn!("bootstrap-log: unknown frame, len={}")`.

Имя таска `"bootstrap-log"`, `Priority::normal()`. Завершение взаимно: смерть rootkeeper'а
дропает peer -> логгер выходит; выход логгера дропает последний Arc local -> rootkeeper
выходит. Логгер stateless (монотонность seq не проверяет — печатает что пришло).

## 4. Q4: init.rs — полный запуск

`spawn_init_process<A>` (crates/kernelspace/src/init.rs):
1. До спавна init-таска собрать только Copy/Send-значения: `launcher: Arc<dyn UserProcessLauncher>
   = Arc::new(SchedulerUserProcessLauncher::new(scheduler.handle(), kernel.address_space_factory()))`
   (образец kernel_tests/mod.rs:96-101), `blob: Option<&'static [u8]> = kernel.userland_blob()`,
   `user_va_end = A::USER_VA_END`, `scheduler_service` через `with_runtime_state`.
2. Вся работа с blob — внутри работающего init-таска (инвариант kernel_tests/mod.rs:133-137:
   удержание разбора до `scheduler.start()` ловило hang; в closure уезжает только Copy-ссылка).
3. В замыкании: `blob == None` -> `warn!("userland blob missing; rootkeeper not started")`,
   return (ядро живёт на idle — idle-поток существует, scheduler.rs:480-487);
   иначе `bootstrap::spawn_rootkeeper(launcher.as_ref(), blob, user_va_end)`:
   `Err` -> `warn!`, return; `Ok(launch)` -> `info!` + `spawn_bootstrap_log(&scheduler_service,
   launch.channel)` (`Err` -> `warn!`). Ни одной паники на userland-пути; `expect` остаётся
   только на спавне самого init-таска (неустранимый сбой bootstrap'а ядра, как сейчас).

**Демо-таски удаляются** (`spawn_demo_processes` целиком: timer-server pilot, test-process,
оба demo-принтера, init.rs:48-91). Обоснование: контракт production-init теперь — запуск
rootkeeper-цепочки; demo-логи каждые 200-700 мс топят секундный heartbeat (подрывают цель
"видимость в klog"); постоянная долбёжка таймера маскирует ровно тот класс багов, который
фиксит w-timer. Дисциплина scope-management (must_fix судьи): удаление — ОТДЕЛЬНЫМ коммитом
внутри w-kernel с фиксацией в чате; откат дешёвый. Модуль `timer_server.rs` НЕ трогаем
(pub API библиотечного крейта, dead-code warnings не появятся); осиротевшие
`spawn_timer_server`/`pilot_*` — задокументированная out-of-scope находка для чата.
В qemu-suite это ничего не меняет: production-init не компилируется в прогон kernel-tests.

## 5. Q5: тесты — все детерминированные

Принципы (нулевая терпимость к флакам):
- Ни одного sleep-поллинга. Каждое ожидание — `object_wait_one` по сигналу; bounded timeout —
  детектор провала, не синхронизация (хэппи-пас будится сигналом за миллисекунды).
- Ассерты на порядок/содержимое (seq ровно 0, затем 1), никогда — на тайминги.
- Явный teardown каждого E2E: `handle_close(chan_id)` + drop всех Arc local ->
  PEER_CLOSED сигнально будит rootkeeper из 1с-сна -> bounded ожидание `THREAD_TERMINATED`
  на `info.thread_object` (must_fix "minimal": без этого EL0-процесс утекает между тестами —
  ровно leak-state-паттерн текущего hang'а).
- Exit-код rootkeeper НЕ ассертится: честная гонка "Timeout сработал vs peer закрылся"
  (wait вернул -9, peer закрылся, write упал) — оба пути exit(0), но ассерт кода = закладка флака.
- kassert-фейл = немедленный exit(1) QEMU (macros.rs:7-13) — 5-секундные бюджеты не суммируются.
- Оговорка: bounded-timeout как детектор провала сам ходит через таймерный wake-путь —
  до w-timer провальный сценарий может проявляться как hang, а не FAIL; хэппи-пасы
  сигнальные и от таймера не зависят (кроме heartbeat-E2E, см. ниже).

5.1 Host-тесты:
- `userland-abi/src/bootstrap.rs`: `parse_bootstrap_heartbeat` valid/short/bad-magic;
  `encode -> parse` round-trip (seq 0, 1, u64::MAX); взаимная неперепутываемость магий
  (hello-байты в heartbeat-парсер и наоборот). Существующие hello-тесты не трогаются.
- `crates/syscall/src/numbers.rs` (tests): guard-тесты двух ABI-зеркал.
- `kernelspace/src/bootstrap.rs` (tests): `classify_frame` — hello/heartbeat/unknown,
  включая префикс-коллизии (10 и 16 байт мусора).

5.2 Rework `userland_rootkeeper_bootstrap_handshake` (hal-aarch64/src/kernel_tests/userland_bootstrap.rs):
- спавн через `kernelspace::bootstrap::spawn_rootkeeper(user_process_launcher().as_ref(),
  blob, Aarch64Context::USER_VA_END)` — production-путь вместо копии моста/канала;
- `install_handle(local: READ|WAIT|INSPECT)`; `object_wait_one(chan_id,
  READABLE|PEER_CLOSED, Some(5 * BOOTSTRAP_HEARTBEAT_PERIOD_NS))`; kassert `observed & READABLE != 0`;
  `channel_read` -> `parse_bootstrap_hello` -> текущие ассерты version/magic;
- teardown: `handle_close(chan_id)` + drop(local) -> `install_handle(KObject::Thread(
  info.thread_object), WAIT|INSPECT)` -> `object_wait_one(THREAD_TERMINATED, Some(5 * PERIOD))`.
Хэппи-пас полностью сигнальный (hello пишется до первого сна rootkeeper'а; wake тест-потока —
signal-path) — тест зеленеет ДО фикса таймера и разблокирует сейчас висящий suite.

5.3 Новый `userland_rootkeeper_heartbeat` (тот же файл): спавн и hello как в 5.2; затем
дважды: `object_wait_one(READABLE|PEER_CLOSED, Some(5 * PERIOD))` -> kassert READABLE ->
`channel_read` -> `parse_bootstrap_heartbeat` -> `kassert_eq!(seq, 0)`, затем `kassert_eq!(seq, 1)`;
teardown как в 5.2. Номинал ~2.1 с. Единственный тест, зависящий от w-timer (секундный тик —
timer-driven пробуждение EL0-потока при полностью спящей системе).

5.4 Новый kernel-тест `bootstrap_log_exits_on_peer_close`
(crates/kernelspace/src/kernel_tests/bootstrap_log.rs, mod в kernel_tests/mod.rs):
`create_pair(0)`; `Event::new()`; спавн таска `move || { run_bootstrap_log(&local);
event.signal(EVENT_SIGNALED, 0) }`; `peer.write(hello-байты, собранные локально)`,
`peer.write(encode_bootstrap_heartbeat(7))`, `peer.write(мусор)`; `drop(peer)`; handle-based
`object_wait_one(EVENT_SIGNALED, Some(5e9))` (образец event_via_scheduler.rs). Покрывает
дренаж, unknown-ветку и выход по PEER_CLOSED чисто сигнально — без user-процесса и таймера.
Event-обёртка живёт в тест-замыкании: production-API без test-only параметров.

5.5 Runner: оба userland-теста добавить в `should_run_early`
(crates/test-harness-qemu/src/runner.rs:94-108) — спавнят полноценный user-процесс/AS,
заявленный критерий раннего батча.

5.6 Production-прогон (закрывает дыру "production-init не исполняется в qemu-suite"):
в acceptance w-kernel — ручной `timeout 15 cargo xtask build devices/spec/qemu-aarch64.yaml
--run` с проверкой `rootkeeper: hello v1` и >=2 строк `rootkeeper: heartbeat #` (0 и 1).

Бюджет времени qemu-suite: handshake-rework — с до-5с поллинга до ~миллисекунд;
heartbeat — +~2.1 с; bootstrap_log — миллисекунды. Итоговая дельта ~ +2.2 с, suite остаётся
в секундах, `--timeout 20` НЕ увеличивается (hang лечится поиском дедлока, не таймаутом).

## 6. Q6: связи модулей

```
userland-abi  (seam: SyscallOp, формат образа, RKHELLO/RKBEAT, PERIOD_NS,
  |            зеркала CHANNEL_SIGNAL_PEER_CLOSED / SYSCALL_RETURN_TIMEOUT)
  |                       ^                ^                   ^
  |  guard-тесты зеркал   |                |                   |
  +--- crates/syscall ----+      userland/rootkeeper    kernelspace/bootstrap.rs
       (видит kobject и ABI)     (EL0: svc 0x11/0x21/0x52)   (spawn_rootkeeper,
                                                              bootstrap-log, classify_frame)
                                          ^                    ^      ^         ^
                                bootstrap-канал (пара)         |      |         |
                                                     userspace |  init.rs   hal-aarch64/
                                                     (мост)  --+ (production) kernel_tests/
                                                                              userland_bootstrap.rs
                                                                              (E2E, Aarch64Context::USER_VA_END)
```

Arch-конкретика (`USER_VA_END`) появляется только в generic `spawn_init_process<A>` и в
hal-aarch64-тестах. Workstreams — см. структурный блок: w-abi -> {w-rootkeeper, w-kernel}
параллельно; w-tests собирает всё (handshake-коммит сдаётся до w-timer); w-timer —
параллельный трек, эксклюзивный владелец крейта scheduler и hal-таймерного пути;
w-verify — финальная приёмка. Файловые множества попарно не пересекаются;
`kernelspace/src/kernel_tests/mod.rs` принадлежит w-kernel,
`hal-aarch64/src/kernel_tests/mod.rs` — w-timer (регистрация его regression-модуля);
w-tests правит только уже зарегистрированный userland_bootstrap.rs и runner.rs.

## 7. Q7: что сознательно НЕ делаем

Полный список — в rejected_entities. Ключевое: никаких новых syscall'ов (sleep, debug-log),
никакого Event-тикера, никакого frame-enum/encode_hello в ABI, зеркала только двух реально
потребляемых констант, никакого userland-runtime-крейта, никакой stateful-логики в логгере,
не ассертим exit-код и klog-строки, не растим qemu-таймаут, не трогаем timer_server.rs.

## 8. Риски

- R1 (гонка Timeout vs PEER_CLOSED): exit(0) на обоих путях + запрет ассертов exit-кода.
- R2 (очередь при мёртвом логгере): capacity 16, write вернёт ошибку -> rootkeeper выйдет.
- R3 (production-init не исполняется в qemu-suite): цикл логгера покрыт kernel-тестом 5.4,
  мост и spawn-цепочка — E2E через общий spawn_rootkeeper; плюс ручной --run в acceptance w-kernel.
- R4 (w-timer затягивается): все остальные WS мержатся раньше; единственный гейт —
  heartbeat-тест (второй коммит w-tests).
- R5 (до w-timer провал bounded-wait может выглядеть как hang, а не FAIL): принято осознанно;
  хэппи-пасы сигнальные, qemu-runner имеет внешний --timeout 20 как последний рубеж.

## Приложение A. Диагноз зависания userland_rootkeeper_bootstrap_handshake (2026-06-09)

Root cause (подтверждён двумя независимыми следователями и арбитром, фикс верифицирован
3/3 прогонами qemu-test): off-by-one на стыке семантик интервалов. Interval полуоткрытый
[start, end) (crates/collections/src/interval_set.rs), MemoryRange закрытый [start, end]
(crates/memory/src/memory_range.rs, frame_count = end-start+1). В MemorySetup::prepare
(crates/hal-aarch64/src/memory/memory_setup.rs:154-156) эксклюзивный interval.end
передавался как инклюзивный конец MemoryRange - каждый свободный регион получал один
фантомный фрейм ЗА своей границей. Фантомный фрейм региона под ядром = PA 0x40200000,
нулевая страница образа ядра с exception vectors (VBAR=...40200800). Next-fit курсор
аллокатора к 21-му тесту дошёл до хвоста региона и выдал эту страницу под L3-таблицу
user-AS rootkeeper: PageTable::new() занулил вектора. Первый же SVC из EL0 ушёл в
зануленный вектор: бесконечный шторм исключений EC=0 с замаскированным DAIF, без паники
и вывода. "Любой лог маскирует баг", потому что меняет раскладку образа и место попадания
фантомного фрейма, а не тайминги.

Фикс: конвертация эксклюзивного конца в инклюзивный отступом на страницу назад
(aligned_down(end-1)) при построении free_heap_regions_iter. Заодно устраняет фантомный
фрейм за концом RAM и за initrd.

Сопутствующее укрепление (рекомендация арбитра, верифицировано отдельно 3/3):
global_asm в crates/hal-aarch64/src/boot/protocol/linux_arm64/header.rs открывает
.section .head и не восстанавливает секцию; блок векторов в
crates/hal-aarch64/src/exception/exceptions.rs:52 не задаёт секцию и наследует .head.
Защита страницы векторов держится на случайном делении страницы с _text_start.
Фикс: явная директива .text в global_asm векторов (именно .text, а не .section .text -
последняя ломает Mach-O host-ассемблер на macOS) + .pushsection/.popsection в header.rs.

Зафиксированные остаточные риски (НЕ фиксить в этой работе, вынесены в чат):
- From<MemoryRegion> for MemoryRange (layout.rs:135-139) - та же exclusive->inclusive
  несостыковка, живых использований нет;
- reserve_frames_exact в memory_setup.rs передаёт aligned_up эксклюзивный конец как
  инклюзивный фрейм - перерезервирование на один фрейм (безопасное направление);
- закрытая семантика MemoryRange [start, end] в целом error-prone, миграция на
  полуоткрытую - отдельный рефакторинг;
- gicv3 пишет EOI после handler'а - при context switch из IRQ-handler'а EOI уезжает
  с вытесненным потоком.
