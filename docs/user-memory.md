# Выделение памяти пользовательским процессам

Этот документ описывает интерфейс динамического выделения памяти из EL0
(пользовательского режима): архитектурно-независимый аллокатор, его
интеграцию с `Process`/`MemoryMapper`, syscall-ABI и ограничения
текущей реализации.

> Инициализация статической части user-AS — сегменты загруженного образа
> и user-стек — описана в [`docs/architecture.md`](architecture.md)
> (`Per-process AddressSpace`) и в `crates/userspace/src/image.rs`.
> Здесь идёт речь только про **динамические** регионы, которые процесс
> запрашивает у ядра уже после старта.

## Концепции

```
User-AS:
   [image segments]  …  [free user-VA hole]  …  [user stack]
                        ^^^^^^^^^^^^^^^^^^^
                        обслуживается UserVmAllocator
```

Каждому user-процессу принадлежат:

- `Arc<AddressSpace>` — `MemoryMapper` user-AS, владеющий L0-root и
  всеми выделенными фреймами (kernel-AS используется
  kernel-thread'ами и не имеет user-маппинга);
- `Arc<MutexCell<HandleTable>>` — capability-таблица для KO;
- `Option<Arc<MutexCell<UserVmAllocator>>>` — реестр динамически
  выделенных VA-регионов (новое в этом интерфейсе).

`UserVmAllocator` — архитектурно-нейтральный per-process state, живёт в
крейте `memory`, не знает ни про aarch64, ни про конкретный
`MemoryMapper`. Его задача — отвечать за "свободный" VA внутри
обслуживаемого диапазона и хранить метаданные уже выделенных регионов
для последующих операций над ними.

## Поток выполнения `vm_allocate`

```text
EL0  -- syscall(MemoryAllocate, size, flags)  -->  trap-handler
                                                          |
trap-handler собирает SyscallFrame, вызывает dispatch     |
                                                          v
                                                   sys_memory_allocate
                                                          |
                                runtime().current_user_vm()|
                                                          v
                            UserVmAllocator::allocate     UserVmContext
                            (выделяет VA-диапазон)        ┌────────────┐
                                                          │ mapper     │
                                                          │ allocator  │
                                                          └────────────┘
                                                          v
                            MemoryMapper::map(va, pages, flags)
                                                          |
                                                          v
                            FrameAllocator выделяет 4К-фреймы,
                            mapper маппит их в L0..L3, kernel-side
                            init копирует пустые страницы.
```

На каждом шаге у ошибок есть путь отката:

- `UserVmAllocator::allocate` падает -> ничего не маппится, аллокатор
  остался в исходном состоянии.
- `MemoryMapper::map` падает (например, OOM фреймов) -> выделенный
  диапазон возвращается в free-list через `free`. `MemoryMapper::map`
  **транзакционен**: при ошибке посередине цикла уже замапленные
  страницы откатываются (PTE обнуляются, фреймы возвращаются
  аллокатору, TLB инвалидируется). После `free` диапазон сразу
  доступен для последующих `allocate` и сливается с соседними
  свободными областями.

## Архитектурно-независимый аллокатор

`UserVmAllocator` — type alias над [`RangeAllocator<MemFlags>`]
(`memory::range_allocator`): first-fit free-list с автоматическим
coalesce'ингом соседних свободных диапазонов. Tag хранит текущие флаги
доступа региона.

| Метод | Назначение |
|---|---|
| `new(start, end)` | Обслуживать диапазон `[start, end)` с дефолтной ёмкостью реестра; внутренние таблицы стартуют с малым резервом под обычные несколько регионов. |
| `with_capacity(start, end, capacity)` | То же, но c ограниченным числом одновременно живых регионов; стартовый резерв не превышает явный лимит. |
| `allocate(size: NonZeroUsize, flags)` | Выделить first-fit диапазон. |
| `free(base, size: NonZeroUsize)` | Вернуть диапазон в free-list (с coalesce'ингом). |
| `lookup(base, size: NonZeroUsize)` | Не-мутирующая проверка регистрации региона (пред-валидация перед `mapper.remap`). |
| `set_tag(base, size: NonZeroUsize, flags)` | Обновить флаги уже зарегистрированного региона. |
| `allocated()` | Снимок всех живых регионов (для тестов). |

Параметры размеров принимают `NonZeroUsize`: нулевой размер исключён на
уровне типа, отдельный вариант ошибки `ZeroSize` не нужен.

Image-сегменты и user-стек маппятся `scheduler`-ом напрямую через
`MemoryMapper::map`, в обход аллокатора; в реестре аллокатора живут
только регионы, выданные `vm_allocate`. Free-list обслуживает "дыру"
между концом самого высокого сегмента образа и базой стека.

## Per-process регистрация

`Process::with_user_vm` ставит `UserVmAllocator` на свежесозданный
процесс. `spawn_user_process` собирает аллокатор так:

```rust
fn build_user_vm_allocator(image: &UserImage<'_>) -> Option<UserVmAllocator> {
    let highest_end = image.highest_segment_end()?;
    let stack_base  = image.user_stack_base().ok()?;
    let highest_aligned = PageAlignedVirtualAddress::from_usize(highest_end.as_usize())?;
    if highest_aligned.as_usize() >= stack_base.as_usize() {
        return None;
    }
    Some(UserVmAllocator::new(highest_aligned, stack_base.as_virtual()))
}
```

Если у образа нет дыры между сегментами и стеком (плотная упаковка,
или нет сегментов), аллокатор не создаётся, и `vm_*` syscall'ы вернут
`WrongType`. Это намеренно: процесс заявил, что управляет всем своим
VA сам.

Kernel-процессы (`SpawnAddressSpace::Kernel`/`Inherit` без user-AS)
никогда не получают `UserVmAllocator` — у них нет user-AS, и user-VM
syscall'ы для них некорректны.

## Доступ из syscall-handler-ов

Глобальный `SyscallRuntime` (`install_runtime` -> `Arc<dyn SyscallRuntime>`)
получает новую точку:

```rust
fn current_user_vm(&self) -> Option<UserVmContext>;
```

`UserVmContext` владеет:

- `mapper: Arc<dyn MemoryMapper + Send + Sync>` — клон `Arc` из `AddressSpace::User`;
- `allocator: Arc<MutexCell<UserVmAllocator>>` — клонируется как
  обычный `Arc`, syscall работает с ним вне scheduler-lock.

`SchedulerHandle` получает оба значения через `current_user_vm_pair`,
проверяет, что AS — user-вариант, и клонирует внутренний `Arc` mapper-а
через `AddressSpace::mapper_arc`.

## Syscall ABI

Номера лежат в зарезервированном диапазоне `0x60..=0x6F`:

| `op` | Имя | Аргументы | Возврат |
|------|-----|-----------|---------|
| `0x60` | `MemoryAllocate` | `arg0=size_bytes`, `arg1=flags` | `va` (адрес базы) |
| `0x61` | `MemoryRemap` | `arg0=va`, `arg1=size_bytes`, `arg2=flags` | `0` |
| `0x62` | `MemoryFree` | `arg0=va`, `arg1=size_bytes` | `0` |

`MemoryRemap` назван по операции `MemoryMapper::remap` — он перемаппит
уже выделенный регион с новыми флагами доступа. Имя сохраняет связь с
платформенной реализацией смены прав в page-tables.

`flags` — 8-битный код `UserMemFlags`:

| код | значение | mapping `MemFlags` |
|-----|----------|-----|
| `0` | `ReadWrite` | `MemFlags::user_rw()` |
| `1` | `ReadOnly` | `MemFlags::user_ro()` |
| `2` | `ReadExecute` | `MemFlags::user_rx()` |

Все размеры — байты, кратны странице (4 КБ). Базовый VA `vm_remap`
обязан полностью совпасть с зарегистрированным регионом — частичная
смена флагов не поддерживается.

### Ошибки

Возвращаются как отрицательное `i64` (см. `SyscallError`):

| Код | Условие |
|-----|---------|
| `InvalidArgument` (3) | Размер 0 / не кратен 4К; неизвестный код флагов; VA не page-aligned. |
| `WrongType` (5) | У текущего процесса нет user-AS / `UserVmAllocator`. |
| `OutOfMemory` (13) | Кончился свободный user-VA, реестр регионов или физические фреймы. |
| `NotFound` (14) | `vm_remap`/`vm_free` на не-выделенный регион. |

Численные коды стабильны и являются частью ABI — их сохраняем, чтобы
ABI-юниты в `error.rs` ловили нарушение совместимости.

## Ограничения текущей реализации

1. **Защита целиком на регион.** `vm_remap`/`vm_free` требуют точного
   совпадения `(base, size)` с выделенным регионом.
2. **AArch64 `remap`/`unmap`.** Платформенная реализация инвалидирует
   TLB точечно (по странице, в правильной ASID-области для user-AS).
   Для kernel-AS — broadcast по всем ASID. Промежуточные L1/L2/L3
   таблицы при `unmap` пока не освобождаются (TODO).

## Тестовые точки

| Уровень | Расположение |
|---|---|
| Юнит-тесты `RangeAllocator` | `crates/memory/src/range_allocator.rs` |
| Юнит-тесты преобразования ошибок | `crates/syscall/src/memory.rs` |
| Юнит-тесты ABI кодов | `crates/syscall/src/error.rs`, `numbers.rs` |
| Интеграция со scheduler | `crates/kernelspace/tests/userspace.rs` |

## Связанные документы

- [`docs/architecture.md`](architecture.md) — Per-process AddressSpace, MemoryMapper.
- `crates/kernelspace/src/user_process.rs` — `spawn_user_process`,
  `build_user_vm_allocator`.
- `crates/syscall/src/numbers.rs` — полный реестр syscall'ов.
