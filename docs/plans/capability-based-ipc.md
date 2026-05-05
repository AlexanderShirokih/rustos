# Capability-based IPC: начальная реализация

## Контекст

README.md и `docs/architecture.md` декларируют RustOS Mobile как capability-based realtime микроядро: драйверы и сервисы должны жить в userspace, доступ к ресурсам — через неподделываемые handles с правами, которые можно делегировать, ограничивать и отзывать. Сегодня этого нет:

- **Discovery** реализован как kernel-time сборка через `register_driver!` + `EmbeddedDriversScanner` (`drivers/common/src/scanner.rs`); матчинг идёт по DTB на этапе `kmain` ещё в EL1.
- **"Сервисы"** — это `Arc<dyn Trait>` в `drivers/common/src/capabilities.rs`, индексированные `TypeId`. Один глобальный `Capabilities` стор, без понятия процесса, прав, владельца. См. `Capabilities` (`drivers/common/src/capabilities.rs:120`), `Driver::run` (там же), `run_retry_passes` (`kernel/src/driver_init.rs:37`).
- **Процессная модель** — `Process` имеет id+name+`Arc<AddressSpace>`, `AddressSpace` — заглушка, всё shared. Скедулер с приоритетами и preemption работает (`kernel/src/sched/scheduler.rs`). EL0/SVC не реализованы (`sync_handler` паникует).

Цель этого плана — заложить в EL1 **каркас Kernel Object Model + per-process Handle Table + Channel/Event/Timer KO + wait** так, чтобы:

1. Текущая `Capabilities`-схема и `register_driver!` продолжали работать без поломок во время миграции.
2. Pilot-сервис (Timer) был перенесён на channel-based API end-to-end.
3. Дизайн был совместим с будущим SVC-слоем и user-space — то есть kernel-side функции уже сегодня имеют сигнатуры "как будто это syscalls" и не утекают наружу `Arc<dyn KernelObject>`.

DTB-парсер и driver discovery в этой итерации **остаются как есть**; план только фиксирует контракт будущего device-manager и `Resource` KO как комментарий в коде, без реализации.

## Целевая модель

### Kernel Object Model

Стартовый набор объектов: `Process`, `Thread`, `Channel` (точнее `ChannelEndpoint` — две половинки), `Event`, `Timer`. Все — `Arc<dyn KernelObject>` за handle-таблицей.

- `Koid` — newtype над `NonZeroU64`, монотонический счётчик. Не capability, только идентификация для логов и сравнения.
- `ObjectType` — `#[repr(u8)] enum { Process, Thread, Channel, Event, Timer }`. Быстрый type-check на границе IPC, без `Any`/`TypeId`.
- `Rights` — `bitflags!` (`DUPLICATE | TRANSFER | READ | WRITE | SIGNAL | WAIT | INSPECT | MANAGE_THREAD | MANAGE_PROCESS`). `Rights::defaults_for(ObjectType)` — стартовые права при создании KO.

### Handle Table

`HandleTable` — per-process слот-вектор + intrusive free-list + per-slot generation. `HandleId` (`NonZeroU32`) кодирует пару `(generation, slot_index)` (например, 12 бит generation + 20 бит slot) — recycle одного слота не превращает старый id в валидный.

Single chokepoint проверки: `HandleTable::get(id, need_rights, need_type) -> Result<&Handle, IpcError>` — единственное место, где проверяются права и тип.

Локация в ядре: `Process` владеет `MutexCell<HandleTable>`. Скедулер уже знает `current_thread() -> ThreadId -> ProcessId`, оттуда же будущий SVC-handler возьмёт нужный `Process`. Пока (без SVC) IPC-функции принимают `&Process` явно.

### Channel

- Две `Arc<ChannelEndpoint>`, связанные `Weak` друг на друга. Каждая — самостоятельный KO с собственной inbound-очередью.
- **Bounded** очередь сообщений (capacity фиксируется при создании пары; начальное значение — 16). `write` возвращает `IpcError::ShouldWait` при заполнении — никаких неограниченных allocations в hot path.
- Сигнал `PEER_CLOSED` поднимается на одной половинке, когда другая дропнута; будит ждущих.
- Сообщение: `{ bytes: BoundedVec<u8, INLINE_MAX>, handles: BoundedVec<Handle, MAX_HANDLES_PER_MSG> }`. Стартовые лимиты: `INLINE_MAX=256`, `MAX_HANDLES_PER_MSG=4`. Без zero-copy и scatter-gather до появления `Vmo`.
- **Handle transfer атомарен**: handles удаляются из source-таблицы под её локом, упаковываются в сообщение; на `read` адресат под своим локом регистрирует их в своей таблице. Если сообщение дропнуто (peer closed, очередь сброшена) — handles в нём закрываются, KO живут только пока есть другие ссылки.
- Глобальный порядок локов двух таблиц по `ProcessId.raw()` — единственное место, где один путь берёт два лока.

### Event и Timer

- `Event` — `signal_state: AtomicU32`, API `signal(set, clear)`, `peek`. Базовый сигнальный примитив.
- `Timer` — KO, ассоциированный с deadline; при срабатывании поднимает сигнал `SIGNALED` и будит ожидающих. Реализация на существующей `SleepQueue`/`KernelTimerSource`. API: `set(deadline)`, `cancel()`, читается через `object_wait_one`.

### Wait

`object_wait_one(handle, signals_mask, timeout) -> Result<u32 /* observed signals */, IpcError>` — единая операция ожидания. Реализуется поверх существующей `WaitQueue` (parking thread в waiter-list объекта) и `SleepQueue` (deadline). Кто проснётся первым — снимает запись с другой стороны. Bounded waiter-count per object, короткие критсекции.

`object_wait_many` и Port — отложены.

### Rights enforcement

Только через `HandleTable::get(...)`. Ни один KO-метод не вызывается "снаружи" без предварительного резолва handle с проверкой `need_rights` и `need_type`. Это даёт единственное аудитируемое место.

### Discovery (контрактом, без реализации)

В этой итерации `register_driver!` и `EmbeddedDriversScanner` живут без изменений. План фиксирует только **будущий** контракт device-manager:

- KO `Resource { Mmio { phys, len } | Irq { vector } }` с правами `READ|WRITE|MANAGE`.
- Device-manager — будущий kernel-thread (потом user-space), парсит DTB, выдаёт `Resource`-handles драйверам через Channel.
- Соответствующий модуль `kobject::resource` появляется в виде doc-only заглушки + TODO с грепабельным якорем `// TODO(kobject-discovery)`.

Реальная миграция discovery — отдельный workstream после Phase 3.

## Архитектурные решения

**Где жить коду:** `kernel/src/kobject/`, не отдельный крейт. Причины:

1. KO нужен доступ к `sched::ProcessTable`, `Thread`, `WaitQueue`, `SleepQueue`. Вынос в отдельный крейт потянул бы туда scheduler.
2. `drivers-common` сейчас ниже `kernel` в DAG; новый `kobject` крейт либо создаст цикл, либо дублирует scheduler-абстракции.
3. Когда дойдём до user-space ABI, имеет смысл выделить `kobject-abi` (no_std, без `alloc`, только `Rights`/`ObjectType`/packed `HandleId`/syscall-номера) — но не сейчас.

**Файлы и крейты:**

```
kernel/src/kobject/
    mod.rs           — re-exports + doc-comment про сосуществование с Capabilities
    koid.rs
    object_type.rs
    rights.rs
    errors.rs        — IpcError
    kernel_object.rs — trait KernelObject
    handle.rs        — Handle, HandleId (packed gen|slot)
    handle_table.rs  — HandleTable + free-list + generation
    wait.rs          — SignalState, WaiterList, object_wait_one
    channel.rs       — ChannelEndpoint::create_pair, Message
    event.rs
    timer.rs         — Timer KO поверх KernelTimerSource
```

Изменяемые существующие файлы:

- `kernel/src/sched/process.rs` — `Process` получает `handle_table: MutexCell<HandleTable>`, инициализируется пустой при `ProcessTable::insert`.
- `kernel/src/lib.rs` — `pub mod kobject;`.
- `kernel/src/kmain.rs` — Phase 3: spawn timer-server kernel-thread, переключение pilot на channel-based.
- `drivers/common/src/services/timer.rs` — Phase 3: thin-adapter, который ходит через handle вместо `Arc<dyn TimerService>`. Старый trait сохраняется параллельно, помечается `// TODO(kobject-migration)`.

Никаких изменений в `drivers-common-aarch64`, `drivers-aarch64`, `arch-aarch64`, `aarch64-paging` в этой итерации.

## Скелеты ключевых типов

```rust
// kernel/src/kobject/koid.rs
pub struct Koid(NonZeroU64);
impl Koid { pub fn allocate() -> Self; pub fn raw(self) -> u64; }

// kernel/src/kobject/object_type.rs
#[repr(u8)]
pub enum ObjectType { Process, Thread, Channel, Event, Timer }

// kernel/src/kobject/rights.rs
bitflags! {
    pub struct Rights: u32 {
        const DUPLICATE = 1 << 0;
        const TRANSFER  = 1 << 1;
        const READ      = 1 << 2;
        const WRITE     = 1 << 3;
        const SIGNAL    = 1 << 4;
        const WAIT      = 1 << 5;
        const INSPECT   = 1 << 6;
        const MANAGE_THREAD  = 1 << 7;
        const MANAGE_PROCESS = 1 << 8;
    }
}
impl Rights { pub const fn defaults_for(t: ObjectType) -> Self; }

// kernel/src/kobject/kernel_object.rs
pub trait KernelObject: Send + Sync {
    fn koid(&self) -> Koid;
    fn object_type(&self) -> ObjectType;
    fn signal_state(&self) -> Option<&SignalState> { None }
    fn on_zero_handles(&self) {}
}

// kernel/src/kobject/handle.rs
pub struct HandleId(NonZeroU32); // packed { generation: 12 | slot: 20 }
pub struct Handle {
    object: Arc<dyn KernelObject>,
    rights: Rights,
}

// kernel/src/kobject/handle_table.rs
pub struct HandleTable { /* slots + free_head */ }
impl HandleTable {
    pub fn insert(&mut self, h: Handle) -> Result<HandleId, IpcError>;
    pub fn remove(&mut self, id: HandleId) -> Result<Handle, IpcError>;
    pub fn get(&self, id: HandleId, need: Rights, ty: ObjectType)
        -> Result<&Handle, IpcError>;
    pub fn duplicate(&mut self, id: HandleId, new_rights: Rights)
        -> Result<HandleId, IpcError>;
}

// kernel/src/kobject/channel.rs
pub struct ChannelEndpoint { /* koid, queue, peer Weak, signals, waiters */ }
impl ChannelEndpoint {
    pub fn create_pair(capacity: usize) -> (Arc<Self>, Arc<Self>);
    pub fn write(&self, msg: Message) -> Result<(), IpcError>;
    pub fn read(&self) -> Result<Message, IpcError>; // ShouldWait if empty
}
pub struct Message {
    bytes:   BoundedVec<u8, INLINE_MAX>,
    handles: BoundedVec<Handle, MAX_HANDLES_PER_MSG>,
}

// kernel/src/kobject/event.rs
pub struct Event { /* koid, signals, waiters */ }

// kernel/src/kobject/timer.rs
pub struct Timer { /* koid, deadline, signals, waiters, source: Arc<dyn KernelTimerSource> */ }

// kernel/src/kobject/errors.rs
pub enum IpcError {
    BadHandle, WrongType, AccessDenied,
    ShouldWait, PeerClosed, Timeout,
    BufferTooSmall, MessageTooBig, OutOfHandles,
}
```

Существующие переиспользуемые компоненты:

- `kernel/src/sched/wait_queue.rs::WaitQueue` — основа `WaiterList`.
- `kernel/src/sched/sleep_queue.rs::SleepQueue` — deadline для `object_wait_one`.
- `kernel::sched::KernelTimerSource` — источник для `Timer` KO.
- `collections::Vec` / `collections::BoundedVec` (если нет — добавить в `collections`).
- `MutexCell` из существующего locking-слоя.

## Фазы

### Phase 0 — QEMU integration test harness (подготовка)

Цель: иметь возможность запускать ядерные сценарии в QEMU и получать структурный pass/fail в CI до того, как мы начнём вносить серьёзные изменения в ядро. Без этого верификация Phase 3 (pilot Timer) — ручная.

**Что добавляется:**

1. **Новый крейт `qemu-test-harness`** (no_std, target aarch64-unknown-none):
   - `src/exit.rs` — ARM semihosting exit. `hlt #0xf000` с `w0 = SYS_EXIT (0x18)` и параметром `ADP_Stopped_ApplicationExit (0x20026)` для exit(0); для exit(1) используется `SYS_EXIT_EXTENDED (0x20)` с code=1 (через стек-блок параметров). Никакого `core::panic` не нужно — на panic-handler внутри crate'а делаем UART-дамп `[TEST-FAIL: panic at file:line]` + semihosting exit(1).
   - `src/case.rs` — `pub struct TestCase { name: &'static str, run: fn() }` + макрос `register_test!(name, fn)`, кладущий дескрипторы в линкер-секцию `.tests.kernel` (`__tests_kernel_start`/`__tests_kernel_end` symbols), полностью аналогично существующему `register_driver!` (`drivers/aarch64/src/lib.rs:35`).
   - `src/runner.rs` — `pub fn run_all_tests()`: итерируется по секции, печатает в UART `[TEST-START: name]`, вызывает `run()`, на возврате — `[TEST-PASS: name]`. По завершении всех — `semihosting::exit(0)`. Любой panic во время `run()` -> harness panic-handler -> `exit(1)`.
   - `src/macros.rs` — `kassert!`, `kassert_eq!`, `kassert_ne!`. На провале печатают `[TEST-FAIL: <expr>] at <file>:<line>` и `semihosting::exit(1)` (без `core::panic!`, чтобы не зависеть от panic-handler в production-ядре).
   - Зависимости: `io` (Writer), опционально `log` для удобного `klog!`. **Не зависит** от `kernel`/`arch-aarch64` — чтобы тесты могли существовать без полного ядра.

2. **Команда `cargo xtask qemu-test`**:
   - Принимает имя test-suite (smoke по умолчанию) и опциональный `--filter <substr>`.
   - Собирает тестовый бинарь под `aarch64-unknown-none`.
   - Запускает QEMU с `-semihosting -nographic -no-reboot -machine virt -cpu cortex-a53 -m 128M -kernel <bin>`, под `timeout` 60s.
   - Возвращает QEMU exit code как свой; параллельно tee'ит stdout, ищет `[TEST-FAIL]` для diagnostic вывода.
   - Файлы: `xtask/src/qemu_test.rs` + интеграция в `xtask/src/main.rs`.

3. **Гибридная структура тестов:**
   ```
   tests/qemu/
     smoke/                   — один бинарь, register_test! для всех нефатальных кейсов
       Cargo.toml
       src/main.rs            — #![no_main] #![no_std], _start -> init UART -> run_all_tests
     <isolated_test_name>/    — отдельный крейт для опасных/panic-кейсов
       Cargo.toml
       src/main.rs
   ```
   `tests/qemu/smoke` на старте содержит **meta-test** "harness работает": один `register_test!` который проверяет что UART пишется и semihosting exit срабатывает.

4. **Device spec для тестов:** `devices/spec/qemu-aarch64-test.yaml` — копия `qemu-aarch64.yaml` с добавлением `-semihosting` и без интерактивных run-команд.

5. **Boot-стратегия:** test-бинарь использует **минимальный собственный** `_start` (см. `tests/qemu/smoke/src/main.rs`), не трогая `arch-aarch64` боевой boot. Это даёт быстрые тесты для изолированной логики (вся kobject-механика отлично туда ложится). Тестирование **реального** boot-пути ядра — отдельная Phase 0.5 после Phase 3, не блокирует начальную работу.

**Затрагивает:** новый крейт `qemu-test-harness/`, новая директория `tests/qemu/`, `xtask/src/qemu_test.rs`, `xtask/src/main.rs`, `Cargo.toml` (workspace members), `devices/spec/qemu-aarch64-test.yaml`.

**Тесты:**
- Host (`cargo test -p qemu-test-harness`): только чистая логика (HandleId-pack, форматирование маркеров) — semihosting/asm в host-сборке отключается через `cfg(target_os = "none")`.
- QEMU: `cargo xtask qemu-test smoke` — запускает meta-test, проверяет: exit code 0, в выводе виден `[TEST-PASS: meta]`.

**Acceptance:**
- `cargo xtask qemu-test smoke` на чистом репо возвращает 0, занимает < 10s, печатает один PASS-маркер.
- При искусственном `kassert!(false)` команда возвращает не-0 и в выводе есть `[TEST-FAIL]` с file:line.
- `cargo clippy --workspace --exclude xtask --target aarch64-unknown-none` чистый.

### Phase 1 — Каркас KOM + HandleTable (без IPC, без scheduler-интеграции)

Добавляем: `Koid`, `ObjectType`, `Rights`, `IpcError`, `KernelObject`, `Handle`, `HandleId` (packed gen|slot), `HandleTable`. Никаких реальных KO ещё нет — только trait и таблица.

Затрагивает: только новые файлы в `kernel/src/kobject/` + регистрация модуля в `kernel/src/lib.rs`.

Тесты (host, под `cargo test -p kernel`): insert/remove/duplicate/get-with-rights, subset-rights at duplicate, generation rollover detection, double-close -> `BadHandle`, type mismatch -> `WrongType`, OOM-таблица -> `OutOfHandles`.

QEMU: добавляется кейс `handle_table_basic` в `tests/qemu/smoke` — sanity, что та же логика собирается и работает в `aarch64-unknown-none` цели.

Acceptance: 100% инвариантов `HandleTable` покрыты host-тестами. Никакого `unsafe` вне packing-хелперов `HandleId`. `cargo clippy` чистый. `cargo xtask qemu-test smoke` зелёный.

### Phase 2 — Channel + Event KO (без scheduler-блокировки)

Добавляем: `Event`, `ChannelEndpoint::create_pair`, `Message`, bounded queue, `SignalState`, скелет `WaiterList` с callback-API. Блокирующий wait в этой фазе **симулируется** в тестах через mock-waker — без реальной интеграции со scheduler (это Phase 3).

Затрагивает: `kernel/src/kobject/{channel.rs, event.rs, wait.rs}`. Никаких изменений в `sched/`.

Тесты (host): write/read echo, queue-full -> `ShouldWait`, peer-close -> `PeerClosed` на другом конце, drop сообщения с handles закрывает их, handle transfer перемещает слоты между двумя `HandleTable`, `Event::signal` будит зарегистрированного наблюдателя через mock-waker, two-tables lock order respected.

QEMU: добавляются `channel_echo` и `event_signal` кейсы в smoke-suite.

Acceptance: интеграционный host-тест эмулирует RPC "request -> response" полностью на двух `HandleTable` + одной паре endpoints, без scheduler. `cargo xtask qemu-test smoke` зелёный.

### Phase 3 — Scheduler-интеграция + pilot Timer

Добавляем:

1. Реальный `WaiterList` через `WaitQueue`/`SleepQueue`: `object_wait_one(handle, signals, timeout)` парк `current_thread()` в waiter-list KO + (при finite timeout) в `SleepQueue`. Wake-path снимает запись из обеих очередей.
2. `Timer` KO поверх `KernelTimerSource`.
3. `Process` получает `MutexCell<HandleTable>` (модификация `kernel/src/sched/process.rs:21`).
4. `ProcessTable::insert` инициализирует пустую `HandleTable`.

**Pilot Timer migration:**

- В `kmain.rs` поднимается "timer-server" kernel-thread, который держит ChannelEndpoint и Loop-ом обрабатывает запросы (`CreateTimer`, `SetDeadline`, `Cancel`) от клиентов; на подписанный таймер отдаёт handle обратно через ответное сообщение.
- Существующий `dyn TimerService` (`drivers/common/src/services/timer.rs`) остаётся зарегистрированным в Capabilities. Параллельно публикуется handle на серверный endpoint таймер-сервиса в `Capabilities` (как `Arc<ChannelEndpoint>`) — переходный мост, помеченный `// TODO(kobject-migration)`.
- Один клиент (демо-процесс из `spawn_demo_processes`, `kernel/src/kmain.rs:127`) переписывается с `scheduler_service.sleep_ms` на "получить handle на timer-server endpoint -> послать `CreateTimer{period_ms}` -> `object_wait_one` на полученном Timer-handle -> перевыставить deadline". Остальные демо-процессы остаются на старом API — двойная дорожка работает.

Затрагивает: `kernel/src/kobject/{wait.rs, timer.rs}`, `kernel/src/sched/process.rs`, `kernel/src/sched/process_table.rs` (если выделен), `kernel/src/kmain.rs`, `drivers/common/src/services/timer.rs` (только адаптер + deprecation-комментарий, семантика не меняется).

Тесты:
- Host: добавляется `MockScheduler` shim (по аналогии с уже существующим `MockContext` в arch-тестах). Проверка wait/wake через channel write между двумя "потоками".
- QEMU smoke: `timer_signal_after_deadline` — поднять минимальный scheduler в test-бинаре, создать Timer KO, поставить deadline, дождаться сигнала через `object_wait_one`, ассертить порядок и приблизительное время.
- On-target QEMU (boot-тест боевого ядра): `cargo xtask build devices/spec/qemu-aarch64.yaml --run` под `timeout 30s`, проверяется что pilot-демо-процесс тикает через channel-based Timer (по `info!`-маркерам), остальные демо-процессы (legacy `sleep_ms`) тоже тикают, нет паник. Этот шаг ручной — он не использует semihosting harness, потому что грузит полный boot. Автоматизация полного boot — Phase 0.5.

Acceptance:
- `cargo test --workspace --exclude drivers-aarch64 --exclude arch-aarch64` зелёный.
- `cargo xtask build devices/spec/qemu-aarch64.yaml --run` под `timeout 30s` — pilot-демо тикает, нет паник, нет deadlock-ов.
- `cargo clippy --workspace --exclude xtask --target aarch64-unknown-none` чистый.
- В `kobject/mod.rs` доковый блок описывает "новый код пишется в kobject, legacy Capabilities живёт до миграции последнего сервиса"; все coexistence-точки помечены `// TODO(kobject-migration)`.

## Что НЕ делаем в этой итерации

- **Нет EL0 / user-space.** Всё в EL1; "процесс" — логическая граница.
- **Нет SVC handler.** IPC-функции вызываются прямыми Rust-вызовами в EL1, но с сигнатурами "как будто это syscalls": наружу никогда не утекает `Arc<dyn KernelObject>`, только `HandleId`.
- **Не трогаем `register_driver!` и `EmbeddedDriversScanner`.** Они продолжают грузить драйверы.
- **Не удаляем `Capabilities`-стор.** Он живёт параллельно. Миграция — посервисно, отдельными PR.
- **Нет реализации `Resource` / MMIO capability.** Только doc-anchor на будущее.
- **Нет per-process page tables.** `AddressSpace` остаётся заглушкой.
- **Нет `Job` иерархии, `Port`, `object_wait_many`, `Vmo`, IRQ KO.** Отложено.
- **Не фиксируется ABI `HandleId`.** Layout (gen|slot) — kernel-internal.

## Риски

1. **Bounded queues vs гибкость.** Фиксированные `INLINE_MAX=256` и `capacity=16` мелковаты для будущих flow (block I/O). Это сознательный realtime-компромисс. Mitigation: `capacity` параметризуется per-channel сразу; `INLINE_MAX` поднимется одновременно с появлением `Vmo` (большие payload — через VMO-handle, не inline).
2. **Гранулярность лока HandleTable.** Один `MutexCell` на процесс — простейший корректный вариант для коротких критсекций. Cross-process handle transfer берёт два лока — риск deadlock. Mitigation: глобальный порядок по `ProcessId.raw()`, единая helper-функция `lock_two(a, b)`, аудит каждого вызова.
3. **Сосуществование `Capabilities` и `HandleTable`.** Пересекающиеся понятия -> риск путаницы у контрибьюторов. Mitigation: doc-comment в `kobject/mod.rs` ("новый код — kobject; legacy — Capabilities"), грепабельный якорь `// TODO(kobject-migration)` на каждом coexistence call-site.

## Критические файлы

Новые (Phase 0):
- `qemu-test-harness/Cargo.toml`, `qemu-test-harness/src/{lib,exit,case,runner,macros}.rs`.
- `tests/qemu/smoke/{Cargo.toml, src/main.rs}` — smoke-suite c meta-test'ом, на старте.
- `xtask/src/qemu_test.rs` — host-runner, парсит маркеры и пробрасывает QEMU exit code.
- `devices/spec/qemu-aarch64-test.yaml` — spec с `-semihosting`.

Новые (Phase 1–3):
- `kernel/src/kobject/mod.rs` — модуль-корень + doc про migration.
- `kernel/src/kobject/handle_table.rs` — Phase 1 ядро.
- `kernel/src/kobject/channel.rs` — Phase 2 IPC primitive.
- `kernel/src/kobject/timer.rs` — Phase 3 pilot KO.
- `kernel/src/kobject/wait.rs` — `object_wait_one` + scheduler-интеграция в Phase 3.

Модифицируемые:
- `Cargo.toml` (workspace) — добавить `qemu-test-harness`, `tests/qemu/smoke` в members.
- `xtask/src/main.rs` — регистрация подкоманды `qemu-test`.
- `kernel/src/lib.rs` — `pub mod kobject;`.
- `kernel/src/sched/process.rs:21` — поле `handle_table` в `Process`, конструктор и геттер.
- `kernel/src/kmain.rs:127` — Phase 3, переключение одного demo-процесса на channel-based timer; spawn timer-server.
- `drivers/common/src/services/timer.rs` — Phase 3, doc-deprecation + adapter (опционально), legacy сохраняется.

Переиспользуемое (НЕ модифицируется):
- `kernel/src/sched/wait_queue.rs` — основа waiter-list.
- `kernel/src/sched/sleep_queue.rs` — deadline path.
- `kernel/src/sched/scheduler.rs::current()` — резолв `Process` в Phase 3.
- `KernelTimerSource` — источник для `Timer` KO.

## Verification

После Phase 0:
- `cargo xtask qemu-test smoke` возвращает 0, в выводе один `[TEST-PASS: meta]`, время < 10s.
- При искусственном `kassert!(false)` команда возвращает не-0 и в выводе виден `[TEST-FAIL]` с file:line.
- `cargo clippy --workspace --exclude xtask --target aarch64-unknown-none` чисто.

После Phase 1:
- `cargo test --workspace --exclude drivers-aarch64 --exclude arch-aarch64` — все новые юнит-тесты `kobject::handle_table` зелёные.
- `cargo xtask qemu-test smoke` зелёный (включая `handle_table_basic` кейс).
- `cargo clippy --workspace --exclude xtask --target aarch64-unknown-none` — чисто.
- `cargo fmt --all --check`.

После Phase 2:
- Всё из Phase 1.
- Новый интеграционный host-тест `kobject::tests::pilot_rpc` (две `HandleTable` + Channel pair + Event, RPC echo).
- `cargo xtask qemu-test smoke` зелёный (включая `channel_echo` и `event_signal`).

После Phase 3:
- Всё из Phase 2.
- `cargo xtask qemu-test smoke` зелёный (+ `timer_signal_after_deadline`).
- `timeout 30 cargo xtask build devices/spec/qemu-aarch64.yaml --run` — боевое ядро бутится, pilot-демо-процесс тикает через channel-based Timer (видно по `info!`), остальные демо-процессы (legacy `sleep_ms`) тоже тикают, нет паник. Шаг ручной.
- `cargo deny check` — без новых проблем.
- Ручной аудит: каждый вызов `Capabilities::require_service`/`provide_service` в kernel-коде помечен `// TODO(kobject-migration)` либо явно остаётся legacy.

## Дальше (вне этого плана)

Следующие workstreams (отдельные планы):

1. **Phase 0.5: Boot-path тестирование под harness.** Завести feature-flag `test_main` в `arch-aarch64`/`kernel`, чтобы боевой boot мог звать `run_all_tests()` после инициализации; покрыть driver-init и scheduler bootstrap on-target.
2. SVC handler в `arch/aarch64/src/exception/exceptions.rs:152` + syscall dispatch table + резолв current process через `TPIDR_EL1`.
3. Per-process page tables: настоящий `AddressSpace` поверх `aarch64-paging` вместо заглушки.
4. EL0 entry: первый user-space процесс, выход через ERET с SPSR=EL0t.
5. `Resource` KO + device-manager service: миграция `EmbeddedDriversScanner` в user-space, `register_driver!` уходит.
6. Миграция оставшихся сервисов (Console, Interrupts, Scheduler, Mmio) на channel-based, удаление `drivers/common/src/capabilities.rs`.
