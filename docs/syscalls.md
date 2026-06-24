# Системные вызовы

Syscall-слой принимает 16-битный номер операции и до шести аргументов
`u64`. Основной возврат — `i64`: неотрицательное значение означает
успех, отрицательное значение — `-(SyscallError as u32)`.

Вторичный возврат `u64` используется только операциями, которые явно
указаны в таблицах ниже. `HandleId` — ненулевой `u32`; `0` и значения
вне `u32` считаются неверным аргументом.

## Права

Handle — числовой идентификатор (`u32`) в таблице текущего процесса,
ссылающийся на объект ядра. Вместе со ссылкой handle хранит маску прав —
набор разрешённых операций с этим объектом. Ядро проверяет права handle
при каждом syscall: нет нужного бита — возвращается `AccessDenied`.
Дублирование создаёт второй handle на тот же объект, но только с равным
или меньшим набором прав; расширить права дублированием нельзя.

| Право       |      Бит | Описание                                                                                                                 |
|-------------|---------:|--------------------------------------------------------------------------------------------------------------------------|
| `DUPLICATE` | `1 << 0` | разрешает создать новый handle на тот же объект с подмножеством прав                                                     |
| `TRANSFER`  | `1 << 1` | разрешает передать handle через port                                                                                     |
| `READ`      | `1 << 2` | чтение из объекта: `recv` на port, mapping памяти на чтение, ожидание сигналов, чтение метаданных                        |
| `WRITE`     | `1 << 3` | запись в объект: `send`/`call` на port, mapping памяти на запись, изменение сигналов, управление process/thread, минтинг |
| `EXECUTE`   | `1 << 4` | mapping памяти с правом исполнения                                                                                       |

## Сигналы

Сигналы — это 32-битная маска состояния объекта: каждый бит означает
наступление определённого события (появилось сообщение, объект завершился
и т. п.). `SignalWaitOne` принимает маску интересующих битов и блокирует
вызывающий поток, пока хотя бы один из них не поднимется.

Единственный сигнализуемый capability target — **`Signal`**. Все ждущиеся события выражаются
как `Signal`; `SignalWait*`/`SignalSet` работают только по нему (на любом
другом типе — `WrongType`).

| capability target | Сигнал     |      Бит | Описание          |
|-------------------|------------|---------:|-------------------|
| `Signal`          | `SIGNALED` | `1 << 0` | событие наступило |

**Lifecycle Process/Thread** наблюдается через привязанный `Signal`. `ProcessTerminationSignal` /
`ThreadTerminationSignal` возвращают handle на `Signal`, бит `SIGNALED`
которого означает "завершён". Exit-код читается отдельно (`ProcessExitCode`/`ThreadExitCode`). 
Объект, терминацию которого никто не наблюдает, не аллоцирует `Signal` вовсе.

## Ошибки

| Код | Имя                 | Условие                                                |
|----:|---------------------|--------------------------------------------------------|
|   1 | `BadSyscall`        | неизвестный номер операции                             |
|   2 | `KernelOriginated`  | syscall-trap сделан из kernel-контекста                |
|   3 | `InvalidArgument`   | неверный аргумент, указатель, размер или флаги         |
|   4 | `BadHandle`         | handle закрыт или не существует                        |
|   5 | `WrongType`         | handle указывает на объект другого типа                |
|   6 | `AccessDenied`      | у handle недостаточно прав                             |
|   7 | `ShouldWait`        | операция сейчас не может завершиться                   |
|   8 | `PeerClosed`        | парный port канала закрыт                              |
|   9 | `Timeout`           | истёк тайм-аут ожидания                                |
|  10 | `BufferTooSmall`    | буфер получателя меньше сообщения                      |
|  11 | `MessageTooBig`     | сообщение превышает лимит                              |
|  12 | `OutOfHandles`      | таблица handle процесса заполнена                      |
|  13 | `OutOfMemory`       | не хватает памяти                                      |
|  14 | `NotFound`          | запрошенный VA-регион не найден                        |
|  15 | `Canceled`          | ожидаемый handle закрыт или передан до прихода сигнала |
|  16 | `ResourceExhausted` | бюджет `Resource` исчерпан                             |
|  17 | `Revoked`           | capability отозван: закрыт предок по цепочке деривации |

## Signal

Signal-вызовы — wait/signal API над `Signal`. Так как все ждущиеся события —
это `Signal`, `SignalWaitMany` ждёт "A или B" как набор `Signal`.

|     Op | Имя              | Аргументы                         | Возврат                                | Права            |
|-------:|------------------|-----------------------------------|----------------------------------------|------------------|
| `0x10` | `SignalSet`      | `handle`, `set`, `clear`, `count` | `0`                                    | `WRITE`          |
| `0x11` | `SignalWaitOne`  | `handle`, `signals`, `timeout_ns` | observed mask                          | `READ`           |
| `0x12` | `SignalWaitMany` | `items_va`, `count`, `timeout_ns` | primary=observed mask, secondary=index | `READ` на каждом |
| `0x13` | `SignalCreate`   | —                                 | `signal_h`                             | —                |

`SignalSet` принимает `count` как закрытый `WakeCount`-код:

|    `count` | Значение | Поведение                                     |
|-----------:|----------|-----------------------------------------------|
|        `0` | `None`   | выставить/снять биты, никого не будить        |
|        `1` | `One`    | разбудить одного ждущего в FIFO-порядке       |
| `u64::MAX` | `All`    | разбудить всех ждущих, чья маска пересекается |

Любое другое значение `count` возвращает `InvalidArgument`. Биты
выставляются/снимаются независимо от `count`.

`SignalCreate` создаёт объект `Signal` и регистрирует handle с полным набором
прав в таблице текущего процесса.

`signals` в `SignalWaitOne` берётся из нижних 32 бит аргумента; нулевая
маска возвращает `InvalidArgument`. `timeout_ns == 0` — poll без парковки.
Ненулевой `timeout_ns` — относительный тайм-аут в наносекундах.

`SignalWaitMany.items_va` указывает на массив 8-байтных записей
`[handle: u32, mask: u32]` (little-endian); `count` ограничен `1..=256`,
нулевая маска в записи — `InvalidArgument`.
Возвращает primary-маску сработавшего capability target и индекс записи в `items` в
secondary-регистре. Дубликаты `handle` в `items` допустимы.

Закрытие или передача handle, на котором висит активный
`SignalWaitOne`/`SignalWaitMany`, разбудит ожидающий поток с кодом
`Canceled`. Ожидание привязано к конкретному slot+generation: после
переиспользования слота под новый handle старая регистрация cancel
не срабатывает повторно.

Пример: дождаться завершения дочернего процесса.

```rust
let term_h = process_termination_signal(process_h)?;
let observed = signal_wait_one(term_h, SIGNALED, timeout_ns)?;
if observed & SIGNALED != 0 {
    let code = process_exit_code(process_h)?;
}
```

## Port

`Port` — синхронный rendezvous-IPC примитив: отправитель и получатель
встречаются на одном объекте, а сам кадр сообщения лежит в per-thread
IPC-буфере вызывающего потока. Один кадр несёт
до `IPC_BUFFER_DATA_MAX` байт данных и до `IPC_BUFFER_MAX_CAPS` хэндлов. При
передаче handle он удаляется из таблицы отправителя и появляется в таблице
получателя: отправитель теряет доступ к нему.

|     Op | Имя          | Аргументы              | Возврат              | Права   |
|-------:|--------------|------------------------|----------------------|---------|
| `0x23` | `PortCreate` | —                      | `port_h`             | —       |
| `0x24` | `PortSend`   | `handle`, `timeout_ns` | `0`                  | `WRITE` |
| `0x25` | `PortRecv`   | `handle`, `timeout_ns` | `reply_h` (либо `0`) | `READ`  |
| `0x26` | `PortCall`   | `handle`, `timeout_ns` | `0`                  | `WRITE` |
| `0x27` | `PortReply`  | `reply_handle`         | `0`                  | `WRITE` |

`PortCreate` создаёт один port-объект (стороны симметричны: любой handle
на него можно использовать как локальный и передавать копии другим процессам).

`PortSend` блокирующе отправляет кадр из IPC-буфера текущего потока: ждёт,
пока встречный `recv`/`call` его примет. `PortRecv` блокирующе принимает кадр
в IPC-буфер текущего потока; если встречный был `call`, в возврат кладётся handle
одноразового `Reply`-объекта (иначе `0`). `PortCall` — блокирующий
запрос-ответ: кадр уходит из IPC-буфера, а ответ оказывается в нём же. На
`Reply`-handle сервер вызывает `PortReply`, доставляя кадр из своего
IPC-буфера вызывателю; `Reply` одноразов.

**Тайм-аут (`timeout_ns`).** Блокирующие `PortSend`/`PortRecv`/`PortCall`
ограничивают ожидание встречной стороны параметром `timeout_ns`:

- `PORT_TIMEOUT_INFINITE` (`u64::MAX`) — ждать бессрочно;
- `0` (`PORT_TIMEOUT_POLL`) — не блокироваться: операция либо завершается
  немедленно (встречная сторона уже ждёт), либо возвращает `Timeout`;
- иначе — относительный дедлайн в наносекундах.

При истечении тайм-аута (в т.ч. в режиме poll, когда встречной стороны нет)
syscall возвращает `Timeout` (`SYSCALL_RETURN_TIMEOUT = -9`). Для `PortCall`
тайм-аут покрывает всю операцию (ожидание получателя + ожидание `reply`);
если вызывающая сторона уходит по тайм-ауту, не дождавшись ответа, сервер
при попытке `PortReply` получит `PeerClosed`, а IPC-буфер вызывателя не
будет затронут.

**Доставка badge.** На `PortRecv`/`PortCall` ядро дополнительно
записывает в поле `IpcBuffer.badge` получателя значок (badge) port-хендла
отправителя (`0`, если хендл незаклеймён). Так сервер различает клиентов и
соединения, не создавая отдельных port: сервер
минтит несколько badged-копий своего port-хендла через `HandleDuplicate`
(каждой — свой badge) и раздаёт их клиентам; клиент шлёт через свою копию, а
сервер на приёме читает её badge. Значок пишется получателю всегда (даже при
пустом теле/без caps); на стороне отправителя поле `badge` не читается. Для
`call` значок вызывателя доставляется серверу на recv; ответ (`PortReply`)
badge не несёт.

Содержимое кадра (байты и хэндлы) кодируется в IPC-буфере по адресу, который
возвращает `IpcBufferAddr` (op `0x56`). Wire-формат типизированного IPC поверх
этого транспорта описан в [ipc.md](ipc.md); он не зависит от того, что underlying
доставка синхронна.

Пример: клиент шлёт запрос сервису и ждёт ответ.

```rust
let ipc = ipc_buffer_addr() as *mut IpcBuffer;
// уложить кадр запроса в IPC-буфер
write_request_frame(ipc, request);
// блокирующий запрос-ответ: ответ окажется в том же IPC-буфере
port_call(service_h, PORT_TIMEOUT_INFINITE);
let response = read_response_frame(ipc);
```

## Handle

Handle-вызовы управляют временем жизни handle в таблице текущего
процесса. Закрытие удаляет запись из таблицы; если это была последняя
ссылка на объект, объект освобождается.

|     Op | Имя               | Аргументы                       | Возврат      | Права       |
|-------:|-------------------|---------------------------------|--------------|-------------|
| `0x30` | `HandleClose`     | `handle`                        | `0`          | —           |
| `0x31` | `HandleDuplicate` | `handle`, `new_rights`, `badge` | `new_handle` | `DUPLICATE` |

### Каскадный отзыв при `HandleClose`

`HandleDuplicate` строит граф деривации: копия — производная (ребёнок)
исходного handle. Перенос капы через Port двигает handle целиком, граф
деривации едет с ним и переживает межпроцессную передачу.

`HandleClose` (а также смерть таблицы при завершении процесса) **отзывает всё
поддерево производных** закрываемой капы:

- производные, в том числе в **чужих** таблицах, становятся невалидными —
  любая операция над ними возвращает `Revoked`;
- для memory-капы дополнительно **срываются активные маппинги**
  поддерева (PTE снимаются, range возвращается аллокатору), в том числе в
  чужом адресном пространстве.

Каскад идёт **строго вниз** по графу: сиблинги и предки закрываемой капы
доступ сохраняют. Перенос **корневой** капы (не производной) — безвозвратная
передача владения: грантор узла не держит, отзывать некому.

Два режима раздачи памяти (и любого capability target) выражаются одним примитивом:

- `transfer` корня — handover (отозвать нельзя);
- `duplicate` -> `transfer` производной — отзываемая "аренда": закрыл/умер
  грантор -> производная у получателя отозвана (`Revoked`).

Гранулярность по клиентам: грантор держит на каждого клиента отдельный
промежуточный узел (`duplicate` под клиента, передать клиенту уже его
производную). Закрыл узел клиента — отвалился только он; закрыл корень —
отвалились все.

Закрытие уже отозванного слота допустимо: освобождает слот, капа не
"застревает".

`new_rights` берётся из нижних 32 бит `arg1` (неизвестные биты отбрасываются) и
обязан быть подмножеством прав исходного хендла. `badge` (полные 64 бита `arg2`,
`0` = без значка) применяется по семантике **set-once**:

- источник не заклеймён (`badge == 0`) и `arg2 != 0` — новый хендл заклеймён
  значком `arg2`;
- источник не заклеймён и `arg2 == 0` — новый хендл без значка;
- источник заклеймён и `arg2 == 0` — новый хендл наследует значок источника;
- источник заклеймён и `arg2 != 0` — попытка переклеймить, ошибка `BadHandle`.

Badge — свойство хендла, а не объекта: разные копии одного и того же port
несут разные значки. На приёме сообщения ядро доставляет badge отправителя
получателю (см. [Port](#port)).

Пример: передать в дочерний процесс только право ожидания (без значка).

```rust
let wait_only = handle_duplicate(process_h, Rights::READ | Rights::TRANSFER, 0)?;
handle_close(process_h);

// Сервер минтит badged-копию port под конкретного клиента:
let client_ep = handle_duplicate(port_h, Rights::WRITE, CLIENT_BADGE)?;
```

## Process

Process-вызовы создают процесс, возвращают handle на текущий процесс и
позволяют наблюдать или принудительно завершать процесс по handle.

|     Op | Имя                        | Аргументы                                                                                    | Возврат                | Права                                                                    |
|-------:|----------------------------|----------------------------------------------------------------------------------------------|------------------------|--------------------------------------------------------------------------|
| `0x40` | `ProcessCreate`            | `name_va`, `name_len`                                                                        | `process_h`            | —                                                                        |
| `0x41` | `ProcessSelf`              | —                                                                                            | `process_h`            | —                                                                        |
| `0x42` | `ProcessLoadImage`         | `process_h`, `desc_va`, `desc_len`                                                           | `0`                    | `WRITE` на `process_h`; `WRITE` + права по флагу на каждом region-handle |
| `0x43` | `ProcessExitCode`          | `process_h`                                                                                  | `exit_code` как `u32`  | `READ`                                                                   |
| `0x44` | `ProcessTerminate`         | `process_h`, `exit_code`                                                                     | `0`                    | `WRITE`                                                                  |
| `0x45` | `ProcessStart`             | `process_h`, `entry_pc`, `user_sp`, `arg`, `priority \| (handles_count << 32)`, `handles_va` | `thread_h`             | `WRITE`; `TRANSFER` на bootstrap-handle                                  |
| `0x46` | `ProcessTerminationSignal` | `process_h`                                                                                  | `signal_h` (read-only) | `READ`                                                                   |
| `0x47` | `ProcessResourceSelf`      | —                                                                                            | `resource_h`           | —                                                                        |

`ProcessCreate` копирует имя процесса из user-памяти, требует корректный UTF-8
и ограничение `name_len <= 64` байт. Пустое имя (`name_len == 0`) допустимо.

`ProcessLoadImage` и `ProcessStart` описаны подробно в разделе
[Process Spawning](#process-spawning).

`ProcessStart` создаёт первый поток процесса и возвращает handle на него.
Нижние 8 бит аргумента `priority | (handles_count << 32)` задают
приоритет потока; биты `8..31` зарезервированы и обязаны быть нулевыми;
в старших 32 битах лежит `handles_count` (`0..=32`). Если
`handles_count > 0`, `handles_va` указывает на массив `u32` little-endian
с bootstrap-handle из таблицы loader-процесса. Эти handle
переносятся в новый процесс и становятся его начальным handle-набором.
`entry_pc`, `user_sp` и `arg` записываются в стартовый user-контекст
первого потока как entry point, stack pointer и bootstrap-аргумент
соответственно. Стартуемый процесс наследует метеринг-`Resource`
вызывающего (caller).

`ProcessResourceSelf` ставит свежий handle на метеринг-`Resource` текущего
процесса (с дефолтными правами, включая `WRITE`) — из его бюджета списываются
`MemoryAllocate`/`MemoryCreateVirtual`/`MemoryCreatePhysical`. `WrongType`,
если метеринг-ресурс не задан.

`ProcessTerminate` не используется для self-exit: handle на текущий
процесс возвращает `AccessDenied` даже при `WRITE`. Собственное
завершение идёт через `ThreadExit`; последний поток процесса помечает
процесс завершённым.

`ProcessTerminationSignal` возвращает handle на ленивый bound-`Signal`
процесса (бит `SIGNALED` = "завершён"), материализуя его при первом вызове;
если процесс уже завершён, `Signal` сразу несёт `SIGNALED`. Выданный handle
read-only (`READ`/`DUPLICATE`/`TRANSFER`, без `WRITE`): наблюдатель не может
подделать терминацию.

Типичный сценарий user-spawn состоит из трёх шагов: `ProcessCreate`,
затем `ProcessLoadImage`, затем `ProcessStart`. Между `LoadImage` и
`Start` никакой поток в child-процессе ещё не исполняется.

Пример: передать supervisor право дождаться завершения текущего процесса.

```rust
// --- сторона процесса (report_h - port к supervisor) ---
let process_h = process_self()?;
let wait_h = handle_duplicate(process_h, Rights::READ | Rights::TRANSFER, 0)?;
handle_close(process_h);
// уложить wait_h как cap в IPC-буфер и доставить его supervisor
let ipc = ipc_buffer_addr() as *mut IpcBuffer;
write_cap_frame(ipc, wait_h);
port_send(report_h, PORT_TIMEOUT_INFINITE);

// --- сторона supervisor (monitor_h - тот же port) ---
port_recv(monitor_h, PORT_TIMEOUT_INFINITE);
let monitored_h = read_cap_frame(ipc).expect("peer transferred wait_h");

let term_h = process_termination_signal(monitored_h)?;
let observed = signal_wait_one(term_h, SIGNALED, timeout_ns);
if observed >= 0 && (observed as u32) & SIGNALED != 0 {
    let code = process_exit_code(monitored_h);
}
```

## Thread

Thread-вызовы создают поток в процессе, возвращают handle на текущий
поток и управляют завершением потока.

|     Op | Имя                       | Аргументы                                             | Возврат                | Права   |
|-------:|---------------------------|-------------------------------------------------------|------------------------|---------|
| `0x50` | `ThreadCreate`            | `process_h`, `entry_pc`, `user_sp`, `arg`, `priority` | `thread_h`             | `WRITE` |
| `0x51` | `ThreadSelf`              | —                                                     | `thread_h`             | —       |
| `0x52` | `ThreadExit`              | `exit_code`                                           | не возвращается        | —       |
| `0x53` | `ThreadExitCode`          | `thread_h`                                            | `exit_code` как `u32`  | `READ`  |
| `0x54` | `ThreadTerminate`         | `thread_h`, `exit_code`                               | `0`                    | `WRITE` |
| `0x55` | `ThreadTerminationSignal` | `thread_h`                                            | `signal_h` (read-only) | `READ`  |
| `0x56` | `IpcBufferAddr`           | —                                                     | `ipc_buffer_va`        | —       |

`ThreadTerminate` не используется для self-exit: handle на текущий
поток возвращает `AccessDenied` даже при `WRITE`. Для завершения
текущего потока вызывается `ThreadExit`.

`ThreadTerminationSignal` симметричен `ProcessTerminationSignal`: возвращает
handle на ленивый bound-`Signal` потока (бит `SIGNALED` = "завершён").

`IpcBufferAddr` возвращает user-VA per-thread IPC-буфера текущего потока; для
kernel-потока без буфера — `-(SyscallError)`. Адрес используется как указатель
на `IpcBuffer` перед каждым port-вызовом (см. секцию [Port](#port)).

`exit_code` берётся из младших 32 бит аргумента и читается как `i32`.

Пример: создать worker-поток и дождаться его exit code.

```rust
fn worker_entry() -> ! {
    thread_exit(0)
}

let process_h = process_self()?;
let thread_h = thread_create(process_h, worker_entry as u64, worker_sp, 0, priority)?;

let term_h = thread_termination_signal(thread_h)?;
let observed = signal_wait_one(term_h, SIGNALED, timeout_ns);
if observed >= 0 && (observed as u32) & SIGNALED != 0 {
    let code = thread_exit_code(thread_h);
}
```

## Memory

Memory-вызовы выделяют память и управляют её маппингом в адресное
пространство процесса. Есть два режима:

- **anonymous** (`MemoryAllocate` / `MemoryFree`) — быстрое выделение
  без handle; такую память нельзя передать другому процессу или
  задублировать с другими правами;
- **region handle** (`MemoryCreateVirtual` / `MemoryCreatePhysical`) —
  возвращает handle на регион, который можно замапить, передать через
  port или задублировать с уменьшенными правами.

|     Op | Имя                    | Аргументы                                       | Возврат                                                           | Права                                                                                           |
|-------:|------------------------|-------------------------------------------------|-------------------------------------------------------------------|-------------------------------------------------------------------------------------------------|
| `0x60` | `MemoryCreateVirtual`  | `resource_h`, `size_bytes`, `access_mask`       | `region_h`                                                        | `WRITE` на `Resource`; расходует `size/PAGE` бюджета (`ResourceExhausted` при нехватке)         |
| `0x61` | `MemoryCreatePhysical` | `resource_h`, `pa`, `size_bytes`, `access_mask` | `region_h`                                                        | `WRITE` на `Resource`; диапазон и доступ должны укладываться в ресурс; расходует бюджет ресурса |
| `0x63` | `MemoryMap`            | `region_h`, `size_bytes`, `flags`               | `va`                                                              | `WRITE` и права по `flags`                                                                      |
| `0x64` | `MemoryRemap`          | `va`, `size_bytes`, `flags`                     | `0`                                                               | grant исходного mapping                                                                         |
| `0x65` | `MemoryAllocate`       | `resource_h`, `size_bytes`, `flags`             | `va`                                                              | `WRITE` на `Resource`; расходует `size/PAGE` бюджета (`ResourceExhausted` при нехватке)         |
| `0x66` | `MemoryFree`           | `va`, `size_bytes`                              | `0`                                                               | —                                                                                               |
| `0x67` | `MemoryRegionInspect`  | `region_h`                                      | primary=`size_bytes`, secondary=`(kind_tag << 16) \| access_bits` | `READ`                                                                                          |

`access_mask`: `R=1`, `W=2`, `X=4`; ноль — `InvalidArgument`; высокие биты отбрасываются.
`flags`: `0=ReadWrite`, `1=ReadOnly`, `2=ReadExecute`; другие значения — `InvalidArgument`.

`Resource` — полномочие на минтинг памяти, несущее **бюджет** (счётчик
страниц по 4 KiB). Каждый успешный `MemoryCreateVirtual` / `MemoryAllocate`
атомарно списывает с бюджета ресурса `size_bytes / 4096` страниц (обязана
делиться нацело); `MemoryCreatePhysical` — округление вверх до целого числа
страниц. При нехватке бюджета операция возвращает `ResourceExhausted` и регион
не создаётся. Корневой `Resource` (крупный PA-диапазон) создаётся ядром при
старте и выдаётся bootstrap-процессу как дополнительный initial handle
(индекс 1; индекс 0 - bootstrap-port).

Метеринг-`Resource` разделяется, а не партиционируется: `ProcessStart`
передаёт ребёнку тот же `Resource`, что у caller, а `ProcessResourceSelf`
отдаёт на него handle с `WRITE`. Поэтому всё дерево процессов списывает из
одного общего пула; суб-бюджетов на процесс нет, и ребёнок может исчерпать
бюджет родителя и сиблингов.

`MemoryMap` требует, чтобы `size_bytes` совпадал с полным размером
региона; частичный mapping поддиапазона не поддерживается.

`MemoryMap` привязывает установленный маппинг к капе, через которую он
сделан: маппинг живёт ровно столько, сколько эта капа. `MemoryFree` и
[отзыв капы](#каскадный-отзыв-при-handleclose) идут через единый teardown
(снять PTE + вернуть range аллокатору), поэтому при `HandleClose` региона —
своего или у предка по деривации — активные маппинги поддерева срываются
в том числе кросс-процессно. `MemoryAllocate`-fastpath публичной
капы не имеет: его маппинг снимается только `MemoryFree` или смертью AS.

Пример: временный рабочий буфер.

```rust
let resource_h = process_resource_self()?;
let buf_va = memory_allocate(resource_h, 4096, 0 /* ReadWrite */);

copy_to_user_buffer(buf_va as u64, payload);
memory_remap(buf_va, 4096, 1 /* ReadOnly */);

send_read_only_buffer(buf_va, 4096);
memory_free(buf_va, 4096);
```

Пример: создать регион, записать данные и передать его другому процессу.

```rust
let resource_h = process_resource_self()?;
let region_h = memory_create_virtual(resource_h, 4096, 3 /* R|W */)?;
let shared_va = memory_map(region_h, 4096, 0 /* ReadWrite */);

write_payload(shared_va as *mut u8, payload);

let readonly_h = handle_duplicate(region_h, Rights::READ | Rights::TRANSFER, 0)?;
// уложить readonly_h как cap в IPC-буфер и отправить peer
let ipc = ipc_buffer_addr() as *mut IpcBuffer;
write_cap_frame(ipc, readonly_h);
port_send(peer_h, PORT_TIMEOUT_INFINITE);
```

## Process Spawning

`ProcessLoadImage` и `ProcessStart` дают userspace полноценную роль
process-manager-а: loader сам пишет байты образа во фреймы регионов через
двойной маппинг и одной syscall просит ядро установить эти регионы в
child AS; вторая syscall атомарно стартует первый поток вместе с
bootstrap-handles.

|     Op | Имя                | Аргументы                                                                                    | Возврат    | Права                                                                    |
|-------:|--------------------|----------------------------------------------------------------------------------------------|------------|--------------------------------------------------------------------------|
| `0x42` | `ProcessLoadImage` | `process_h`, `desc_va`, `desc_len` (== `56`)                                                 | `0`        | `WRITE` на `process_h`; `WRITE` + права по флагу на каждом region-handle |
| `0x45` | `ProcessStart`     | `process_h`, `entry_pc`, `user_sp`, `arg`, `priority \| (handles_count << 32)`, `handles_va` | `thread_h` | `WRITE` на `process_h`; `TRANSFER` на каждом bootstrap-handle            |

Для `ProcessLoadImage` необходимые права на region-handle зависят от флага
сегмента: `WRITE | READ` для `RW`, `WRITE | READ` для `RO`, `WRITE | READ | EXECUTE`
для `RX`. Иными словами, `WRITE` на region-handle требуется всегда (он
авторизует установку маппинга в чужой AS).

`ProcessLoadImage` и `ProcessStart` рассчитаны на одноразовый
bootstrap fresh child-процесса. После загрузки образа child уже не
считается "пустым", а после старта первого потока повторный `Start`
также отвергается (`WrongType`).

### Layout `UserImageDescAbi` (56 B)

`desc_va` указывает на little-endian структуру размером `56` байт:

| Offset | Поле              | Тип   | Описание                                                   |
|-------:|-------------------|-------|------------------------------------------------------------|
|      0 | `version`         | `u32` | ABI-версия (`= 1`)                                         |
|      4 | `segment_count`   | `u32` | `1..=16`                                                   |
|      8 | `segments_va`     | `u64` | user-указатель на массив `[UserSegmentAbi; segment_count]` |
|     16 | `entry_va`        | `u64` | PC первой инструкции в child AS                            |
|     24 | `user_stack_top`  | `u64` | вершина стека (4K-выровнена)                               |
|     32 | `user_stack_size` | `u64` | размер стека (4K-кратен, ненулевой)                        |
|     40 | `user_vm_base`    | `u64` | база `user_vm`-аллокатора (4K-выровнена)                   |
|     48 | `user_vm_size`    | `u64` | размер `user_vm`-диапазона                                 |

### Layout `UserSegmentAbi` (32 B)

`segments_va` указывает на массив `segment_count` записей по `32` байта:

| Offset | Поле            | Тип   | Описание                                    |
|-------:|-----------------|-------|---------------------------------------------|
|      0 | `region_handle` | `u32` | `HandleId` региона в loader-таблице         |
|      4 | `flags`         | `u32` | `0=RW`, `1=RO`, `2=RX`                      |
|      8 | `va_base`       | `u64` | база сегмента в child AS (4K-выровнена)     |
|     16 | `mapped_size`   | `u64` | `== region.size_bytes()`                    |
|     24 | `reserved`      | `u64` | `0`                                         |

Некорректная версия ABI, нулевой/слишком большой `segment_count`,
невыровненные адреса, несовпадающий размер региона, ненулевой `reserved`
или несовместимый `access_mask` региона возвращают `InvalidArgument` /
`AccessDenied`.

### Pipeline (loader-side)

```text
PortCreate                                       -> bootstrap_h
resource_h = process_resource_self()             -> resource_h
MemoryCreateVirtual(resource_h, size, R|W|X)     -> region_h
MemoryMap(region_h, size, RW)                    -> seg_va  // в loader-AS
copy image bytes -> seg_va                                  // CPU stores
MemoryRemap(seg_va, size, RX)                               // если нужен RX
ProcessCreate("child")                           -> proc_h
build UserImageDescAbi + [UserSegmentAbi; N] на стеке loader-а
ProcessLoadImage(proc_h, desc_va, 56)            -> 0
MemoryFree(seg_va, size)                                    // фреймы остаются за child
ProcessStart(proc_h, entry_pc, user_sp, arg, prio | (1<<32), &[bootstrap_h])
                                                 -> thread_h
ProcessTerminationSignal(proc_h)                 -> term_h
SignalWaitOne(term_h, SIGNALED, ...)
ProcessExitCode(proc_h)                          -> exit_code
```

### Стоимость

| Syscall            | `copy_user_in` объём                      | Копий образа |
|--------------------|-------------------------------------------|--------------|
| `ProcessLoadImage` | `<= 56 + 16*32 = 568 B` (desc + сегменты) | 0            |
| `ProcessStart`     | `<= 32*4 = 128 B` (HandleId-массив)       | 0            |

Двойной маппинг: тот же регион памяти хранится в loader-AS и child-AS,
ядро лишь записывает PTE в child mapper - данные сегмента копируются
ровно один раз (CPU stores loader).
