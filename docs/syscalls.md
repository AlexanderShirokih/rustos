# Системные вызовы

Syscall-слой принимает 16-битный номер операции и до шести аргументов
`u64`. Основной возврат — `i64`: неотрицательное значение означает
успех, отрицательное значение — `-(SyscallError as u32)`.

Вторичный возврат `u64` используется только операциями, которые явно
указаны в таблицах ниже. `HandleId` — ненулевой `u32`; `0` и значения
вне `u32` считаются неверным аргументом.

Примеры ниже используют Rust-подобные wrapper'ы над ABI: они опускают
упаковку указателей, размеров и secondary return, когда это не важно для
сценария.

## Права

Handle — это capability: он указывает на kernel-object и несёт набор
прав. Дублирование handle'а может только сузить права, но не расширить их.

| Право | Бит | Описание |
|---|---:|---|
| `DUPLICATE` | `1 << 0` | разрешает создать новый handle на тот же объект с подмножеством прав |
| `TRANSFER` | `1 << 1` | разрешает передать handle через channel |
| `READ` | `1 << 2` | чтение из объекта: чтение channel, mapping памяти на чтение |
| `WRITE` | `1 << 3` | запись в объект: запись channel, mapping памяти на запись |
| `SIGNAL` | `1 << 4` | изменение сигналов объекта через `ObjectSignal` |
| `WAIT` | `1 << 5` | ожидание сигналов через `ObjectWaitOne` |
| `INSPECT` | `1 << 6` | чтение метаданных: exit code, memory region info |
| `MANAGE_THREAD` | `1 << 7` | управление thread object |
| `MANAGE_PROCESS` | `1 << 8` | управление process object и создание потоков в процессе |
| `MAP` | `1 << 9` | mapping memory-региона в адресное пространство процесса |
| `EXECUTE` | `1 << 10` | mapping памяти с правом исполнения |
| `MINT` | `1 << 11` | минтинг memory-региона из `PhysicalResource` |

## Сигналы

Сигналы — это 32-битная маска состояния объекта. `ObjectWaitOne`
просыпается, когда хотя бы один ожидаемый бит поднят.

| KO | Сигнал | Бит | Описание |
|---|---|---:|---|
| `Event` | `EVENT_SIGNALED` | `1 << 0` | пользовательское событие произошло |
| `Channel` | `CHANNEL_READABLE` | `1 << 0` | во входящей очереди есть сообщение |
| `Channel` | `CHANNEL_PEER_CLOSED` | `1 << 1` | парный endpoint закрыт |
| `Channel` | `CHANNEL_WRITABLE` | `1 << 2` | в очереди peer'а есть место для записи |
| `Process` | `PROCESS_TERMINATED` | `1 << 0` | процесс завершён |
| `Thread` | `THREAD_TERMINATED` | `1 << 0` | поток завершён |
| `Mailbox` | `MAILBOX_READABLE` | `1 << 0` | в очереди есть хотя бы один пакет |

## Ошибки

| Код | Имя | Условие |
|---:|---|---|
| 1 | `BadSyscall` | неизвестный номер операции |
| 2 | `KernelOriginated` | syscall-trap сделан из kernel-контекста |
| 3 | `InvalidArgument` | неверный аргумент, указатель, размер или флаги |
| 4 | `BadHandle` | handle закрыт или не существует |
| 5 | `WrongType` | handle указывает на объект другого типа |
| 6 | `AccessDenied` | у handle недостаточно прав |
| 7 | `ShouldWait` | операция сейчас не может завершиться |
| 8 | `PeerClosed` | парный endpoint канала закрыт |
| 9 | `Timeout` | ожидание не дождалось сигнала |
| 10 | `BufferTooSmall` | буфер получателя меньше сообщения |
| 11 | `MessageTooBig` | сообщение превышает лимит |
| 12 | `OutOfHandles` | таблица handle'ов процесса заполнена |
| 13 | `OutOfMemory` | не хватает памяти |
| 14 | `NotFound` | запрошенный VA-регион не найден |

## Object

Object-вызовы работают с общей сигнальной частью event, channel,
process и thread. Они нужны для wait/poll-сценариев без знания
конкретного типа объекта на стороне ожидающего кода.

| Op | Имя | Аргументы | Возврат | Права |
|---:|---|---|---|---|
| `0x10` | `ObjectSignal` | `handle`, `set`, `clear` | `0` | `SIGNAL` |
| `0x11` | `ObjectWaitOne` | `handle`, `signals`, `timeout_ns` | observed mask | `WAIT` |

`timeout_ns == 0` — poll без парковки. Ненулевой `timeout_ns` —
относительный таймаут в наносекундах.

Пример: дождаться сообщения в канале.

```rust
let observed = object_wait_one(
    channel_h,
    CHANNEL_READABLE | CHANNEL_PEER_CLOSED,
    timeout_ns,
)?;

if observed & CHANNEL_READABLE != 0 {
    let mut bytes = [0u8; MESSAGE_CAP];
    let mut handles = [0; 4];

    let (bytes_len, handles_count) = channel_read(
        channel_h,
        /* bytes */ &mut bytes,
        /* handles */ &mut handles,
    )?;
    let message = &bytes[..bytes_len];
    let attached_handles = &handles[..handles_count];
}
```

## Channel

Channel — двусторонний IPC-эндпоинт. Сообщение несёт до 256 байт и до
4 handle'ов. Передача handle'а переносит capability из таблицы
отправителя в таблицу получателя.

| Op | Имя | Аргументы | Возврат | Права |
|---:|---|---|---|---|
| `0x20` | `ChannelCreate` | — | primary=`left_h`, secondary=`right_h` | — |
| `0x21` | `ChannelWrite` | `handle`, `bytes_va`, `bytes_len`, `handles_va`, `handles_count` | `0` | `WRITE`, `TRANSFER` на handle'ах |
| `0x22` | `ChannelRead` | `handle`, `bytes_va`, `bytes_cap`, `handles_va`, `handles_cap` | `bytes_len \| (handles_count << 32)` | `READ` |

Массив handle'ов в user-памяти — последовательность `u32` little-endian.
Если receive-буферы малы, `ChannelRead` возвращает `BufferTooSmall`, а
сообщение остаётся в очереди.

Пример: RPC-запрос с ответным event.

```rust
let (left_h, right_h) = channel_create()?;

let reply_event_h: HandleId = existing_reply_event_h;
let transfer_handles = [reply_event_h];

channel_write(
    right_h,
    /* bytes */ request,
    /* handles */ &transfer_handles,
)?;

let observed = object_wait_one(
    left_h,
    CHANNEL_READABLE | CHANNEL_PEER_CLOSED,
    timeout_ns,
)?;
if observed & CHANNEL_PEER_CLOSED != 0 {
    return Err(Error::PeerClosed);
}

let mut response = [0u8; RESPONSE_CAP];
let mut received_handles = [0; 4];

let (bytes_len, handles_count) = channel_read(
    left_h,
    /* bytes */ &mut response,
    /* handles */ &mut received_handles,
)?;

let response = &response[..bytes_len];
let handles = &received_handles[..handles_count];
```

## Handle

Handle-вызовы управляют lifecycle capability в текущем процессе.
Закрытие удаляет запись из таблицы; если это была последняя ссылка на
объект, объект освобождается.

| Op | Имя | Аргументы | Возврат | Права |
|---:|---|---|---|---|
| `0x30` | `HandleClose` | `handle` | `0` | — |
| `0x31` | `HandleDuplicate` | `handle`, `new_rights` | `new_handle` | `DUPLICATE` |

Пример: передать в дочерний процесс только право ожидания.

```rust
let wait_only = handle_duplicate(
    process_h,
    Rights::WAIT | Rights::INSPECT | Rights::TRANSFER,
)?;

channel_write(
    control_h,
    /* bytes */ &[],
    /* handles */ &[wait_only],
)?;
```

## Process

Process-вызовы создают процесс, возвращают handle на текущий процесс и
позволяют наблюдать или принудительно завершать процесс по handle.

| Op | Имя | Аргументы | Возврат | Права |
|---:|---|---|---|---|
| `0x40` | `ProcessCreate` | `name_va`, `name_len` | `process_h` | — |
| `0x41` | `ProcessSelf` | — | `process_h` | — |
| `0x43` | `ProcessExitCode` | `process_h` | `exit_code` как `u32` | `INSPECT` |
| `0x44` | `ProcessTerminate` | `process_h`, `exit_code` | `0` | `MANAGE_PROCESS` |

`ProcessTerminate` не используется для self-exit: handle на текущий
процесс возвращает `AccessDenied` даже при `MANAGE_PROCESS`. Собственное
завершение идёт через `ThreadExit`; последний поток процесса поднимает
`PROCESS_TERMINATED`.

Пример: передать supervisor'у право дождаться завершения текущего процесса.

```rust
let process_h = process_self()?;
let wait_h = handle_duplicate(
    process_h,
    Rights::WAIT | Rights::INSPECT | Rights::TRANSFER,
)?;

channel_write(
    supervisor_h,
    /* bytes */ &[],
    /* handles */ &[wait_h],
)?;
handle_close(process_h)?;

let observed = object_wait_one(received_process_h, PROCESS_TERMINATED, timeout_ns)?;
if observed & PROCESS_TERMINATED != 0 {
    let code = process_exit_code(received_process_h)?;
}
```

## Thread

Thread-вызовы создают поток в процессе, возвращают handle на текущий
поток и управляют завершением потока.

| Op | Имя | Аргументы | Возврат | Права |
|---:|---|---|---|---|
| `0x50` | `ThreadCreate` | `process_h`, `entry_pc`, `user_sp`, `arg`, `priority` | `thread_h` | `MANAGE_PROCESS` |
| `0x51` | `ThreadSelf` | — | `thread_h` | — |
| `0x52` | `ThreadExit` | `exit_code` | не возвращается | — |
| `0x53` | `ThreadExitCode` | `thread_h` | `exit_code` как `u32` | `INSPECT` |
| `0x54` | `ThreadTerminate` | `thread_h`, `exit_code` | `0` | `MANAGE_THREAD` |

`ThreadTerminate` также не используется для self-exit: handle на текущий
поток возвращает `AccessDenied` даже при `MANAGE_THREAD`. Для завершения
текущего потока вызывается `ThreadExit`.

`exit_code` берётся из младших 32 бит аргумента и читается как `i32`.

Пример: создать worker-поток в текущем процессе и прочитать его код.

```rust
let process_h = process_self()?;
let thread_h = thread_create(process_h, worker_pc, worker_sp, worker_arg, priority)?;

fn worker_entry() -> ! {
    thread_exit(/* exit_code */ 0)
}

let observed = object_wait_one(thread_h, THREAD_TERMINATED, timeout_ns)?;
if observed & THREAD_TERMINATED != 0 {
    let code = thread_exit_code(thread_h)?;
}
```

## Memory

Memory-вызовы дают процессу страницы памяти и capability на регионы
памяти. Есть два режима: простой anonymous allocation без handle'а и
работа с `KObject::Memory`, который можно инспектировать, дублировать и
передавать.

| Op | Имя | Аргументы | Возврат | Права |
|---:|---|---|---|---|
| `0x60` | `MemoryCreateVirtual` | `size_bytes`, `access_mask` | `region_h` | — |
| `0x61` | `MemoryCreatePhysical` | `resource_h`, `pa`, `size_bytes`, `access_mask` | `region_h` | `MINT` на `PhysicalResource` |
| `0x63` | `MemoryMap` | `region_h`, `size_bytes`, `flags` | `va` | `MAP` и нужный доступ |
| `0x64` | `MemoryRemap` | `va`, `size_bytes`, `flags` | `0` | grant исходного mapping'а |
| `0x65` | `MemoryAllocate` | `size_bytes`, `flags` | `va` | — |
| `0x66` | `MemoryFree` | `va`, `size_bytes` | `0` | — |
| `0x67` | `MemoryRegionInspect` | `region_h` | primary=`size_bytes`, secondary=`(kind << 16) \| access_bits` | `INSPECT` |

`access_mask`: `R=1`, `W=2`, `X=4`. `flags`: `0=ReadWrite`,
`1=ReadOnly`, `2=ReadExecute`.

Пример: временный рабочий буфер.

```rust
let buf_va = memory_allocate(/* size_bytes */ 4096, UserMemFlags::ReadWrite)?;

copy_to_user_buffer(buf_va, payload)?;
memory_remap(buf_va, /* size_bytes */ 4096, UserMemFlags::ReadOnly)?;

send_read_only_buffer(buf_va, /* size_bytes */ 4096)?;
memory_free(buf_va, /* size_bytes */ 4096)?;
```

Пример: создать регион и передать его другому процессу.

```rust
let region_h = memory_create_virtual(
    /* size_bytes */ 4096,
    AccessMask::R | AccessMask::W,
)?;
let shared_va = memory_map(
    region_h,
    /* size_bytes */ 4096,
    UserMemFlags::ReadWrite,
)?;

let readonly_region_h = handle_duplicate(
    region_h,
    Rights::MAP | Rights::READ | Rights::INSPECT | Rights::TRANSFER,
)?;

channel_write(
    peer_h,
    /* bytes */ &[],
    /* handles */ &[readonly_region_h],
)?;
```

## Mailbox

Mailbox — many-to-one сборщик пакетов фиксированного размера. Источники
двух видов: user-код (явный `MailboxQueue`) и сигналы других KO
(подписка через `MailboxWaitAsync`). Получатель блокируется на
`MailboxWait` и достаёт один пакет за вызов; маршрутизация — по
пользовательскому `key`, выданному при подписке.

| Op | Имя | Аргументы | Возврат | Права |
|---:|---|---|---|---|
| `0x70` | `MailboxCreate` | — | `mailbox_h` | — |
| `0x71` | `MailboxQueue` | `mailbox_h`, `packet_va`, `packet_len` | `0` | `WRITE` на mailbox |
| `0x72` | `MailboxWait` | `mailbox_h`, `timeout_ns`, `packet_va`, `packet_cap` | `packet_len` | `READ` на mailbox |
| `0x73` | `MailboxWaitAsync` | `mailbox_h`, `target_h`, `key`, `mask_and_mode` | `0` | `WRITE` на mailbox, `WAIT` на target |
| `0x74` | `MailboxCancel` | `mailbox_h`, `target_h`, `key` | `0` | `WRITE` на mailbox |

Пакет — 32 байта, layout `repr(C)`:

| Offset | Поле | Тип | Описание |
|---:|---|---|---|
| 0 | `key` | `u64` | произвольное значение, выданное user-ом |
| 8 | `kind` | `u8` | `0=User`, `1=SignalOnce`, `2=SignalRepeating` |
| 12 | `status` | `i32` | резерв, сейчас всегда `0` |
| 16 | `payload` | `[u8; 16]` | данные пакета |

`MailboxQueue` принимает только `kind=0` (User-пакет): signal-пакеты
ставит ядро при срабатывании подписки, попытка проставить `kind=1`/`2`
от user отвергается с `InvalidArgument`. Для signal-пакетов
`payload[0..4]` — маска подписки в little-endian, `payload[4..8]` —
наблюдённый набор сигналов на момент wake.

`MailboxWaitAsync.mask_and_mode` упаковывает маску сигналов в нижние
32 бита, режим — в верхние: `0=Once` (одноразовая доставка, observer
автоматически снимается после wake), `1=Repeating` (доставка на каждое
срабатывание сигнала, до явного `MailboxCancel` или дропа
mailbox/target). `MailboxCancel` идемпотентен: «нет такой подписки»
тоже возвращает `0`.

`Mailbox` в качестве `target_h` отвергается с `WrongType` — иначе
циклические подписки (`A→B→A` или один mailbox через два handle от
`HandleDuplicate`) приводят к рекурсивному захвату внутреннего лока на
пути доставки.

Очередь пакетов ограничена. На полной очереди:

- `MailboxQueue` от user возвращает `ShouldWait` без побочных эффектов;
- signal-пакет от подписки тихо дропается, наращивая внутренний счётчик
  переполнений (доступ к нему придёт отдельным inspect-вызовом).

Пример: event-loop сервиса, реагирующего на несколько источников.
Сервис обслуживает запросы из канала клиента, периодический watchdog
через `Event` и завершение worker-потока. Без Mailbox каждый источник
требовал бы отдельного `ObjectWaitOne` (последовательный poll или
дополнительные потоки). С Mailbox — одна точка ожидания и маршрутизация
по `key`.

```rust
const KEY_REQUEST: u64 = 1;
const KEY_WATCHDOG: u64 = 2;
const KEY_WORKER_DONE: u64 = 3;

let mailbox_h = mailbox_create()?;

mailbox_wait_async(
    mailbox_h, client_channel_h, KEY_REQUEST,
    CHANNEL_READABLE, AsyncMode::Repeating,
)?;
mailbox_wait_async(
    mailbox_h, watchdog_event_h, KEY_WATCHDOG,
    EVENT_SIGNALED, AsyncMode::Repeating,
)?;
mailbox_wait_async(
    mailbox_h, worker_thread_h, KEY_WORKER_DONE,
    THREAD_TERMINATED, AsyncMode::Once,
)?;

loop {
    let packet = mailbox_wait(mailbox_h, /* timeout_ns */ 0)?;
    match packet.key {
        KEY_REQUEST => handle_client_request(client_channel_h)?,
        KEY_WATCHDOG => reset_watchdog(watchdog_event_h)?,
        KEY_WORKER_DONE => {
            let code = thread_exit_code(worker_thread_h)?;
            return finalize(code);
        }
        _ => {}
    }
}
```

Один поток обслуживает все источники, без busy-poll'а и
дополнительных потоков-наблюдателей. `Repeating` подходит для
постоянных событийных потоков (запросы, периодические сигналы),
`Once` — для one-shot уведомлений (завершение потока, ответ на конкретный
запрос). Добавление нового источника — одна строка `mailbox_wait_async`,
без правки структуры цикла.
