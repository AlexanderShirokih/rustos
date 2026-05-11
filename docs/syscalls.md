# Системные вызовы

Syscall-слой принимает 16-битный номер операции и до шести аргументов
`u64`. Основной возврат — `i64`: неотрицательное значение означает
успех, отрицательное значение — `-(SyscallError as u32)`.

Вторичный возврат `u64` используется только операциями, которые явно
указаны в таблицах ниже. `HandleId` — ненулевой `u32`; `0` и значения
вне `u32` считаются неверным аргументом.

Примеры ниже используют Rust-подобные wrapper'ы над ABI: они опускают
упаковку указателей, размеров и secondary return, когда это неважно для
сценария.

## Права

Handle — числовой идентификатор (`u32`) в таблице текущего процесса,
ссылающийся на объект ядра. Вместе со ссылкой handle хранит маску прав —
набор разрешённых операций с этим объектом. Ядро проверяет права handle'а
при каждом syscall: нет нужного бита — возвращается `AccessDenied`.
Дублирование создаёт второй handle на тот же объект, но только с равным
или меньшим набором прав; расширить права дублированием нельзя.

| Право            |       Бит | Описание                                                             |
|------------------|----------:|----------------------------------------------------------------------|
| `DUPLICATE`      |  `1 << 0` | разрешает создать новый handle на тот же объект с подмножеством прав |
| `TRANSFER`       |  `1 << 1` | разрешает передать handle через channel                              |
| `READ`           |  `1 << 2` | чтение из объекта: чтение channel, mapping памяти на чтение          |
| `WRITE`          |  `1 << 3` | запись в объект: запись channel, mapping памяти на запись            |
| `SIGNAL`         |  `1 << 4` | изменение сигналов объекта через `ObjectSignal`                      |
| `WAIT`           |  `1 << 5` | ожидание сигналов через `ObjectWaitOne`                              |
| `INSPECT`        |  `1 << 6` | чтение метаданных: exit code, memory region info                     |
| `MANAGE_THREAD`  |  `1 << 7` | управление thread object                                             |
| `MANAGE_PROCESS` |  `1 << 8` | управление process object и создание потоков в процессе              |
| `MAP`            |  `1 << 9` | mapping memory-региона в адресное пространство процесса              |
| `EXECUTE`        | `1 << 10` | mapping памяти с правом исполнения                                   |
| `MINT`           | `1 << 11` | минтинг memory-региона из `PhysicalResource`                         |

## Сигналы

Сигналы — это 32-битная маска состояния объекта: каждый бит означает
наступление определённого события (появилось сообщение, объект завершился
и т. п.). Чтобы дождаться события, передайте маску интересующих битов в
`ObjectWaitOne` — вызов заблокируется, пока хотя бы один из них не поднимется.

| KO        | Сигнал                |      Бит | Описание                               |
|-----------|-----------------------|---------:|----------------------------------------|
| `Event`   | `EVENT_SIGNALED`      | `1 << 0` | пользовательское событие произошло     |
| `Channel` | `CHANNEL_READABLE`    | `1 << 0` | во входящей очереди есть сообщение     |
| `Channel` | `CHANNEL_PEER_CLOSED` | `1 << 1` | парный endpoint закрыт                 |
| `Channel` | `CHANNEL_WRITABLE`    | `1 << 2` | в очереди peer'а есть место для записи |
| `Process` | `PROCESS_TERMINATED`  | `1 << 0` | процесс завершён                       |
| `Thread`  | `THREAD_TERMINATED`   | `1 << 0` | поток завершён                         |
| `Mailbox` | `MAILBOX_READABLE`    | `1 << 0` | в очереди есть хотя бы один пакет      |

## Ошибки

| Код | Имя                | Условие                                                |
|----:|--------------------|--------------------------------------------------------|
|   1 | `BadSyscall`       | неизвестный номер операции                             |
|   2 | `KernelOriginated` | syscall-trap сделан из kernel-контекста                |
|   3 | `InvalidArgument`  | неверный аргумент, указатель, размер или флаги         |
|   4 | `BadHandle`        | handle закрыт или не существует                        |
|   5 | `WrongType`        | handle указывает на объект другого типа                |
|   6 | `AccessDenied`     | у handle недостаточно прав                             |
|   7 | `ShouldWait`       | операция сейчас не может завершиться                   |
|   8 | `PeerClosed`       | парный endpoint канала закрыт                          |
|   9 | `Timeout`          | ожидание не дождалось сигнала                          |
|  10 | `BufferTooSmall`   | буфер получателя меньше сообщения                      |
|  11 | `MessageTooBig`    | сообщение превышает лимит                              |
|  12 | `OutOfHandles`     | таблица handle'ов процесса заполнена                   |
|  13 | `OutOfMemory`      | не хватает памяти                                      |
|  14 | `NotFound`         | запрошенный VA-регион не найден                        |
|  15 | `Canceled`         | ожидаемый handle закрыт или передан до прихода сигнала |

## Object

Object-вызовы — универсальный wait/signal API: один и тот же код умеет
ждать на channel, event, process или thread, не зная конкретный тип
объекта. Используйте их, когда нужно ждать сигнала от произвольного
handle или программно поднять/снять сигнал на объекте.

|     Op | Имя              | Аргументы                         | Возврат                                | Права            |
|-------:|------------------|-----------------------------------|----------------------------------------|------------------|
| `0x10` | `ObjectSignal`   | `handle`, `set`, `clear`          | `0`                                    | `SIGNAL`         |
| `0x11` | `ObjectWaitOne`  | `handle`, `signals`, `timeout_ns` | observed mask                          | `WAIT`           |
| `0x12` | `ObjectWaitMany` | `items_va`, `count`, `timeout_ns` | primary=observed mask, secondary=index | `WAIT` на каждом |

`timeout_ns == 0` — poll без парковки. Ненулевой `timeout_ns` —
относительный тайм-аут в наносекундах.

`ObjectWaitMany.items_va` указывает на массив 8-байтных записей
`[handle: u32, mask: u32]` (little-endian); `count` ограничен 256.
Возвращает primary-маску сработавшего KO и индекс записи в `items` в
secondary-регистре. Дубликаты `handle` в `items` допустимы.

Закрытие или передача handle'а, на котором висит активный
`ObjectWaitOne`/`ObjectWaitMany`, разбудит ожидающий поток с кодом
`Canceled`. Ожидание привязано к конкретному slot+generation: после
переиспользования слота под новый handle старая регистрация cancel
не срабатывает повторно.

Пример: дождаться сообщения в канале.

```rust
let observed = object_wait_one(
    channel_h,
    CHANNEL_READABLE | CHANNEL_PEER_CLOSED,
    timeout_ns,
)?;

if observed & CHANNEL_PEER_CLOSED != 0 {
    return Err(Error::PeerClosed);
}

let mut bytes = [0u8; MESSAGE_CAP];
let mut handles = [0; 4];
let (bytes_len, handles_count) = channel_read(channel_h, &mut bytes, &mut handles)?;
let message = &bytes[..bytes_len];
let attached_handles = &handles[..handles_count];
```

## Channel

Channel — двусторонний канал для передачи сообщений между процессами.
Одно сообщение несёт до 256 байт данных и до 4 handle'ов. При передаче
handle'а он удаляется из таблицы отправителя и появляется в таблице
получателя: отправитель теряет доступ к нему.

|     Op | Имя             | Аргументы                                                        | Возврат                               | Права                            |
|-------:|-----------------|------------------------------------------------------------------|---------------------------------------|----------------------------------|
| `0x20` | `ChannelCreate` | —                                                                | primary=`left_h`, secondary=`right_h` | —                                |
| `0x21` | `ChannelWrite`  | `handle`, `bytes_va`, `bytes_len`, `handles_va`, `handles_count` | `0`                                   | `WRITE`, `TRANSFER` на handle'ах |
| `0x22` | `ChannelRead`   | `handle`, `bytes_va`, `bytes_cap`, `handles_va`, `handles_cap`   | `bytes_len \| (handles_count << 32)`  | `READ`                           |

Массив handle'ов в user-памяти — последовательность `u32` little-endian.
Если receive-буферы малы, `ChannelRead` возвращает `BufferTooSmall`, а
сообщение остаётся в очереди.

Пример: клиент отправляет запрос сервису и ждёт ответа.

```rust
let (client_h, service_h) = channel_create()?;

// передаём один конец сервису через bootstrap-channel
channel_write(bootstrap_h, &[], &[service_h])?;

// отправляем запрос
channel_write(client_h, request, &[])?;

// ждём ответа
let observed = object_wait_one(
    client_h,
    CHANNEL_READABLE | CHANNEL_PEER_CLOSED,
    timeout_ns,
)?;
if observed & CHANNEL_PEER_CLOSED != 0 {
    return Err(Error::PeerClosed);
}

let mut response = [0u8; RESPONSE_CAP];
let (bytes_len, _) = channel_read(client_h, &mut response, &mut [])?;
let response = &response[..bytes_len];
```

## Handle

Handle-вызовы управляют временем жизни handle'ов в таблице текущего
процесса. Закрытие удаляет запись из таблицы; если это была последняя
ссылка на объект, объект освобождается.

|     Op | Имя               | Аргументы              | Возврат      | Права       |
|-------:|-------------------|------------------------|--------------|-------------|
| `0x30` | `HandleClose`     | `handle`               | `0`          | —           |
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

|     Op | Имя                | Аргументы                                                                                    | Возврат               | Права                                                                          |
|-------:|--------------------|----------------------------------------------------------------------------------------------|-----------------------|--------------------------------------------------------------------------------|
| `0x40` | `ProcessCreate`    | `name_va`, `name_len`                                                                        | `process_h`           | —                                                                              |
| `0x41` | `ProcessSelf`      | —                                                                                            | `process_h`           | —                                                                              |
| `0x42` | `ProcessLoadImage` | `process_h`, `desc_va`, `desc_len`                                                           | `0`                   | `MANAGE_PROCESS`, `MAP`/`READ`/`WRITE`/`EXECUTE` на memory-handle'ах сегментов |
| `0x43` | `ProcessExitCode`  | `process_h`                                                                                  | `exit_code` как `u32` | `INSPECT`                                                                      |
| `0x44` | `ProcessTerminate` | `process_h`, `exit_code`                                                                     | `0`                   | `MANAGE_PROCESS`                                                               |
| `0x45` | `ProcessStart`     | `process_h`, `entry_pc`, `user_sp`, `arg`, `priority \| (handles_count << 32)`, `handles_va` | `thread_h`            | `MANAGE_PROCESS`; `TRANSFER` на bootstrap-handle'ах                            |

`ProcessCreate` копирует имя процесса из user-памяти, требует
непустой корректный UTF-8 и ограничение `1 <= name_len <= 64` байт.

`ProcessLoadImage` загружает user-образ в уже созданный, но ещё не
запущенный процесс. `desc_len` обязан быть ровно `56`
([`USER_IMAGE_DESC_SIZE`](../crates/syscall/src/spawn_abi.rs)), а
`desc_va` — указывать на little-endian структуру:

| Offset | Поле              | Тип   | Описание                            |
|-------:|-------------------|-------|-------------------------------------|
|    `0` | `version`         | `u32` | версия ABI, сейчас только `1`       |
|    `4` | `segment_count`   | `u32` | число сегментов, `1..=16`           |
|    `8` | `segments_va`     | `u64` | указатель на массив сегментов       |
|   `16` | `entry_va`        | `u64` | PC первой user-инструкции           |
|   `24` | `user_stack_top`  | `u64` | вершина user-стека                  |
|   `32` | `user_stack_size` | `u64` | размер user-стека                   |
|   `40` | `user_vm_base`    | `u64` | база диапазона user-vm-аллокатора   |
|   `48` | `user_vm_size`    | `u64` | размер диапазона user-vm-аллокатора |

`segments_va` указывает на массив `segment_count` записей по `32` байта
([`USER_SEGMENT_SIZE`](../crates/syscall/src/spawn_abi.rs)):

| Offset | Поле            | Тип   | Описание                                             |
|-------:|-----------------|-------|------------------------------------------------------|
|    `0` | `region_handle` | `u32` | handle memory-региона в таблице loader-процесса      |
|    `4` | `flags`         | `u32` | `0 = RW`, `1 = RO`, `2 = RX`                         |
|    `8` | `va_base`       | `u64` | база mapping'а в child AS                            |
|   `16` | `mapped_size`   | `u64` | размер mapping'а; должен совпасть с размером региона |
|   `24` | `reserved`      | `u64` | обязано быть `0`                                     |

Для каждого сегмента loader обязан передать `region_handle` с правом
`MAP` и правами доступа, совместимыми с `flags`: `READ|WRITE` для `RW`,
`READ` для `RO`, `READ|EXECUTE` для `RX`. Некорректная версия ABI,
нулевой/слишком большой `segment_count`, невыровненные адреса, несовпадающий
размер региона или ненулевой `reserved` возвращают `InvalidArgument`.
Кроме того, `ProcessLoadImage` принимает только fresh child-процесс:
образ ещё не загружен, потоков ещё нет и child handle-таблица ещё пуста.
Нарушение этих precondition'ов возвращает `WrongType`.

`ProcessStart` создаёт первый поток процесса и возвращает handle на него.
Нижние 8 бит аргумента `priority | (handles_count << 32)` задают
приоритет потока; биты `8..31` зарезервированы и обязаны быть нулевыми;
в старших 32 битах лежит `handles_count` (`0..=32`). Если
`handles_count > 0`, `handles_va` указывает на массив `u32` little-endian
с bootstrap-handle'ами из таблицы loader-процесса. Эти handle'ы
переносятся в новый процесс и становятся его начальным handle-набором.
`entry_pc`, `user_sp` и `arg` записываются в стартовый user-контекст
первого потока как entry point, stack pointer и bootstrap-аргумент
соответственно. Как и `ProcessLoadImage`, `ProcessStart` работает только
на fresh child-процессе с уже загруженным образом, нулевым числом
потоков и пустой child handle-table; иначе возвращает `WrongType`.

`ProcessTerminate` не используется для self-exit: handle на текущий
процесс возвращает `AccessDenied` даже при `MANAGE_PROCESS`. Собственное
завершение идёт через `ThreadExit`; последний поток процесса поднимает
`PROCESS_TERMINATED`.

Типичный сценарий user-spawn состоит из трёх шагов: `ProcessCreate`,
затем `ProcessLoadImage`, затем `ProcessStart`. Между `LoadImage` и
`Start` никакой поток в child-процессе ещё не исполняется.

Пример: передать supervisor'у право дождаться завершения текущего процесса.

```rust
// --- сторона процесса ---
let process_h = process_self()?;
let wait_h = handle_duplicate(
    process_h,
    Rights::WAIT | Rights::INSPECT | Rights::TRANSFER,
)?;
channel_write(supervisor_h, &[], &[wait_h])?;
handle_close(process_h)?;

// --- сторона supervisor'а ---
let observed = object_wait_one(received_process_h, PROCESS_TERMINATED, timeout_ns)?;
if observed & PROCESS_TERMINATED != 0 {
    let code = process_exit_code(received_process_h)?;
}
```

Пример: создать дочерний процесс, загрузить образ и запустить первый поток.

```rust
let code_region_h = memory_create_virtual(
    PAGE_SIZE,
    AccessMask::R | AccessMask::W | AccessMask::X,
)?;
let child_h = process_create("child", 5)?;

let image = UserImageDesc {
    version: 1,
    segments: &[UserSegmentDesc {
        region_handle: code_region_h,
        flags: UserMemFlags::ReadExecute,
        va_base: CHILD_CODE_VA,
        mapped_size: PAGE_SIZE,
    }],
    entry: CHILD_CODE_VA,
    user_stack_top: CHILD_STACK_TOP,
    user_stack_size: PAGE_SIZE,
    user_vm_base: CHILD_USER_VM_BASE,
    user_vm_size: CHILD_USER_VM_SIZE,
};

process_load_image(child_h, &image)?;
let first_thread_h = process_start(
    child_h,
    CHILD_CODE_VA,
    CHILD_STACK_TOP,
    /* arg */ 0,
    /* priority */ 1,
    /* bootstrap handles */ &[],
)?;
```

## Thread

Thread-вызовы создают поток в процессе, возвращают handle на текущий
поток и управляют завершением потока.

|     Op | Имя               | Аргументы                                             | Возврат               | Права            |
|-------:|-------------------|-------------------------------------------------------|-----------------------|------------------|
| `0x50` | `ThreadCreate`    | `process_h`, `entry_pc`, `user_sp`, `arg`, `priority` | `thread_h`            | `MANAGE_PROCESS` |
| `0x51` | `ThreadSelf`      | —                                                     | `thread_h`            | —                |
| `0x52` | `ThreadExit`      | `exit_code`                                           | не возвращается       | —                |
| `0x53` | `ThreadExitCode`  | `thread_h`                                            | `exit_code` как `u32` | `INSPECT`        |
| `0x54` | `ThreadTerminate` | `thread_h`, `exit_code`                               | `0`                   | `MANAGE_THREAD`  |

`ThreadTerminate` также не используется для self-exit: handle на текущий
поток возвращает `AccessDenied` даже при `MANAGE_THREAD`. Для завершения
текущего потока вызывается `ThreadExit`.

`exit_code` берётся из младших 32 бит аргумента и читается как `i32`.

Пример: создать worker-поток и дождаться его exit code.

```rust
fn worker_entry() -> ! {
    thread_exit(/* exit_code */ 0)
}

let process_h = process_self()?;
let thread_h = thread_create(process_h, worker_entry as usize, worker_sp, 0, priority)?;

let observed = object_wait_one(thread_h, THREAD_TERMINATED, timeout_ns)?;
if observed & THREAD_TERMINATED != 0 {
    let code = thread_exit_code(thread_h)?;
}
```

## Memory

Memory-вызовы выделяют память и управляют её маппингом в адресное
пространство процесса. Есть два режима:

- **anonymous** (`MemoryAllocate` / `MemoryFree`) — быстрое выделение
  без handle'а; такую память нельзя передать другому процессу или
  задублировать с другими правами;
- **region handle** (`MemoryCreateVirtual` / `MemoryCreatePhysical`) —
  возвращает handle на регион, который можно замапить, передать через
  channel или задублировать с уменьшенными правами.

|     Op | Имя                    | Аргументы                                       | Возврат                                                       | Права                                                                        |
|-------:|------------------------|-------------------------------------------------|---------------------------------------------------------------|------------------------------------------------------------------------------|
| `0x60` | `MemoryCreateVirtual`  | `size_bytes`, `access_mask`                     | `region_h`                                                    | —                                                                            |
| `0x61` | `MemoryCreatePhysical` | `resource_h`, `pa`, `size_bytes`, `access_mask` | `region_h`                                                    | `MINT` на `PhysicalResource`; диапазон и доступ должны укладываться в ресурс |
| `0x63` | `MemoryMap`            | `region_h`, `size_bytes`, `flags`               | `va`                                                          | `MAP` и нужный доступ                                                        |
| `0x64` | `MemoryRemap`          | `va`, `size_bytes`, `flags`                     | `0`                                                           | grant исходного mapping'а                                                    |
| `0x65` | `MemoryAllocate`       | `size_bytes`, `flags`                           | `va`                                                          | —                                                                            |
| `0x66` | `MemoryFree`           | `va`, `size_bytes`                              | `0`                                                           | —                                                                            |
| `0x67` | `MemoryRegionInspect`  | `region_h`                                      | primary=`size_bytes`, secondary=`(kind << 16) \| access_bits` | `INSPECT`                                                                    |

`access_mask`: `R=1`, `W=2`, `X=4`. `flags`: `0=ReadWrite`,
`1=ReadOnly`, `2=ReadExecute`.

`MemoryMap` требует, чтобы `size_bytes` совпадал с полным размером
региона; частичный mapping поддиапазона сейчас не поддерживается.

Пример: временный рабочий буфер.

```rust
let buf_va = memory_allocate(/* size_bytes */ 4096, UserMemFlags::ReadWrite)?;

copy_to_user_buffer(buf_va, payload)?;
memory_remap(buf_va, /* size_bytes */ 4096, UserMemFlags::ReadOnly)?;

send_read_only_buffer(buf_va, /* size_bytes */ 4096)?;
memory_free(buf_va, /* size_bytes */ 4096)?;
```

Пример: создать регион, записать данные и передать его другому процессу только на чтение.

```rust
let region_h = memory_create_virtual(/* size_bytes */ 4096, AccessMask::R | AccessMask::W)?;
let shared_va = memory_map(region_h, /* size_bytes */ 4096, UserMemFlags::ReadWrite)?;

write_payload(shared_va, payload);

let readonly_region_h = handle_duplicate(
    region_h,
    Rights::MAP | Rights::READ | Rights::INSPECT | Rights::TRANSFER,
)?;
channel_write(peer_h, &[], &[readonly_region_h])?;
```

## Process Spawning

`ProcessLoadImage` и `ProcessStart` дают userspace полноценную роль
process-manager-а: loader сам пишет байты образа во фреймы регионов через
двойной маппинг и одной syscall просит ядро установить эти регионы в
child AS; вторая syscall атомарно стартует первый поток вместе с
bootstrap-handles.

|     Op | Имя                | Аргументы                                                                                    | Возврат    | Права                                                                            |
|-------:|--------------------|----------------------------------------------------------------------------------------------|------------|----------------------------------------------------------------------------------|
| `0x42` | `ProcessLoadImage` | `process_h`, `desc_va`, `desc_len` (== `56`)                                                 | `0`        | `MANAGE_PROCESS` на `process_h`; для каждого региона — `MAP \| (R/W/X по flags)` |
| `0x45` | `ProcessStart`     | `process_h`, `entry_pc`, `user_sp`, `arg`, `priority \| (handles_count << 32)`, `handles_va` | `thread_h` | `MANAGE_PROCESS` на `process_h`; `TRANSFER` на каждом bootstrap-handle           |

### Layout `UserImageDescAbi` (56 B)

| Offset | Поле              | Тип   | Описание                                                   |
|-------:|-------------------|-------|------------------------------------------------------------|
|      0 | `version`         | `u32` | ABI-версия (`= 1`)                                         |
|      4 | `segment_count`   | `u32` | `1..=16`                                                   |
|      8 | `segments_va`     | `u64` | user-указатель на массив `[UserSegmentAbi; segment_count]` |
|     16 | `entry_va`        | `u64` | PC первой инструкции в child AS                            |
|     24 | `user_stack_top`  | `u64` | вершина стека (4K-выровнена)                               |
|     32 | `user_stack_size` | `u64` | размер стека (4K-кратен, ≠ 0)                              |
|     40 | `user_vm_base`    | `u64` | база `user_vm`-аллокатора (4K-выровнена)                   |
|     48 | `user_vm_size`    | `u64` | размер `user_vm`-диапазона                                 |

### Layout `UserSegmentAbi` (32 B)

| Offset | Поле            | Тип   | Описание                                    |
|-------:|-----------------|-------|---------------------------------------------|
|      0 | `region_handle` | `u32` | `HandleId` региона в loader-таблице         |
|      4 | `flags`         | `u32` | `0=RW`, `1=RO`, `2=RX` (см. `UserMemFlags`) |
|      8 | `va_base`       | `u64` | база сегмента в child AS (4K-выровнена)     |
|     16 | `mapped_size`   | `u64` | `== region.size_bytes()`                    |
|     24 | `reserved`      | `u64` | `0`                                         |

`ProcessLoadImage` и `ProcessStart` не являются общими операциями
редактирования/рестарта процесса: обе syscall рассчитаны на одноразовый
bootstrap fresh child-процесса. После загрузки образа child уже не
считается "пустым", а после старта первого потока повторный `Start`
также отвергается.

### Pipeline (loader-side):

```text
ChannelCreate                                    -> (parent_h, child_h)
MemoryCreateVirtual(size, R|W|X access_mask)     -> region_h
MemoryMap(region_h, size, RW)                    -> seg_va  // в loader-AS
copy image bytes -> seg_va                                 // CPU stores
MemoryRemap(seg_va, size, RX)                              // если нужен RX
ProcessCreate("child")                           -> proc_h
build UserImageDescAbi + [UserSegmentAbi; N] на стеке loader-а
ProcessLoadImage(proc_h, desc_va, 56)            -> 0
MemoryFree(seg_va, size)                                   // фреймы остаются за child через Arc<MemoryRegion>
ProcessStart(proc_h, entry_pc, user_sp, arg, prio | (1<<32), &[child_h])
                                                 -> thread_h
ObjectWaitOne(proc_h, PROCESS_TERMINATED, ...)
ProcessExitCode(proc_h)                          -> exit_code
```

### Стоимость

| Syscall            | `copy_user_in` объём                     | Копий образа |
|--------------------|------------------------------------------|--------------|
| `ProcessLoadImage` | `≤ 56 + 16*32 = 568 B` (desc + сегменты) | 0            |
| `ProcessStart`     | `≤ 32*4 = 128 B` (HandleId-массив)       | 0            |

Двойной маппинг: тот же `Arc<MemoryRegion>` хранится в loader's и child's
`UserVmAllocator`, ядро лишь записывает PTE в child mapper — данные
сегмента копируются ровно один раз (CPU stores loader'а).

## Mailbox

Mailbox решает задачу event-loop'а: один поток ждёт сразу на нескольких
объектах. Вместо отдельного `ObjectWaitOne` на каждый объект вы
подписываете их через `MailboxWaitAsync`, а затем крутите один
`MailboxWait`. Когда любой из объектов поднимает ожидаемый сигнал, ядро
кладёт пакет в mailbox; `MailboxWait` возвращает его вместе с `key`,
по которому вы определяете источник. Положить пакет вручную, без сигнала,
можно через `MailboxQueue`.

|     Op | Имя                | Аргументы                                            | Возврат      | Права                                |
|-------:|--------------------|------------------------------------------------------|--------------|--------------------------------------|
| `0x70` | `MailboxCreate`    | —                                                    | `mailbox_h`  | —                                    |
| `0x71` | `MailboxQueue`     | `mailbox_h`, `packet_va`, `packet_len`               | `0`          | `WRITE` на mailbox                   |
| `0x72` | `MailboxWait`      | `mailbox_h`, `timeout_ns`, `packet_va`, `packet_cap` | `packet_len` | `READ` на mailbox                    |
| `0x73` | `MailboxWaitAsync` | `mailbox_h`, `target_h`, `key`, `mask_and_mode`      | `0`          | `WRITE` на mailbox, `WAIT` на target |
| `0x74` | `MailboxCancel`    | `mailbox_h`, `target_h`, `key`                       | `0`          | `WRITE` на mailbox                   |

Пакет — 32 байта, layout `repr(C)`:

| Offset | Поле      | Тип        | Описание                                      |
|-------:|-----------|------------|-----------------------------------------------|
|      0 | `key`     | `u64`      | произвольное значение, выданное user-ом       |
|      8 | `kind`    | `u8`       | `0=User`, `1=SignalOnce`, `2=SignalRepeating` |
|     12 | `status`  | `i32`      | резерв, сейчас всегда `0`                     |
|     16 | `payload` | `[u8; 16]` | данные пакета                                 |

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
