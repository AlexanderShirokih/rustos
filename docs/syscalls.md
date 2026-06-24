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
| `TRANSFER`       |  `1 << 1` | разрешает передать handle через port                            |
| `READ`           |  `1 << 2` | чтение из объекта: `recv` на port, mapping памяти на чтение, ожидание сигналов, чтение метаданных |
| `WRITE`          |  `1 << 3` | запись в объект: `send`/`call` на port, mapping памяти на запись, изменение сигналов, управление process/thread, минтинг |
| `EXECUTE`        |  `1 << 4` | mapping памяти с правом исполнения                                   |

## Сигналы

Сигналы — это 32-битная маска состояния объекта: каждый бит означает
наступление определённого события (появилось сообщение, объект завершился
и т. п.). Чтобы дождаться события, передайте маску интересующих битов в
`SignalWaitOne` — вызов заблокируется, пока хотя бы один из них не поднимется.

Единственный сигнализуемый KO — **`Signal`**. Все ждущиеся события выражаются
как `Signal`; `SignalWait*`/`SignalSet` работают только по нему (на любом
другом типе — `WrongType`).

| KO       | Сигнал     |      Бит | Описание                |
|----------|------------|---------:|-------------------------|
| `Signal` | `SIGNALED` | `1 << 0` | событие наступило       |

**Lifecycle Process/Thread** наблюдается через привязанный (bound) `Signal`,
а не через зашитые в объект сигналы. `ProcessTerminationSignal` /
`ThreadTerminationSignal` возвращают handle на ленивый `Signal`, бит `SIGNALED`
которого означает «завершён»; exit-код читается отдельно
(`ProcessExitCode`/`ThreadExitCode`). Объект, термнинацию которого никто не
наблюдает, не аллоцирует `Signal` вовсе.

`Port` и `Reply` не сигнализуемы и в `SignalWait*` не участвуют: они
используют синхронную rendezvous-парковку, а не сигнальную маску.

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
|   8 | `PeerClosed`       | парный port канала закрыт                          |
|   9 | `Timeout`          | истёк тайм-аут ожидания (Signal poll, блокирующий Port)|
|  10 | `BufferTooSmall`   | буфер получателя меньше сообщения                      |
|  11 | `MessageTooBig`    | сообщение превышает лимит                              |
|  12 | `OutOfHandles`     | таблица handle'ов процесса заполнена                   |
|  13 | `OutOfMemory`      | не хватает памяти                                      |
|  14 | `NotFound`         | запрошенный VA-регион не найден                        |
|  15 | `Canceled`         | ожидаемый handle закрыт или передан до прихода сигнала |
|  16 | `ResourceExhausted`| бюджет `Resource` исчерпан                             |
|  17 | `Revoked`          | капа отозвана: закрыт предок по цепочке деривации      |

## Signal

Signal-вызовы — wait/signal API над `Signal`. Так как все ждущиеся события —
это `Signal`, `SignalWaitMany` по-прежнему ждёт «A ИЛИ B» как набор `Signal`'ов
(термнинация и сообщение — просто два разных `Signal`'а).

|     Op | Имя              | Аргументы                              | Возврат                                | Права            |
|-------:|------------------|----------------------------------------|----------------------------------------|------------------|
| `0x10` | `SignalSet`   | `handle`, `set`, `clear`, `count`      | `0`                                    | `WRITE`          |
| `0x11` | `SignalWaitOne`  | `handle`, `signals`, `timeout_ns`      | observed mask                          | `READ`           |
| `0x12` | `SignalWaitMany` | `items_va`, `count`, `timeout_ns`      | primary=observed mask, secondary=index | `READ` на каждом |
| `0x13` | `SignalCreate`   | —                                      | `signal_h`                             | —                |

`SignalSet.count == 0` — будит всех waiter'ов, у которых маска пересекается
с `set`. `count == N` (N > 0) — будит не более N в FIFO-порядке.

`SignalCreate` создаёт объект `Signal` и регистрирует handle с полным набором
прав в таблице текущего процесса.

`timeout_ns == 0` — poll без парковки. Ненулевой `timeout_ns` —
относительный тайм-аут в наносекундах.

`SignalWaitMany.items_va` указывает на массив 8-байтных записей
`[handle: u32, mask: u32]` (little-endian); `count` ограничен 256.
Возвращает primary-маску сработавшего KO и индекс записи в `items` в
secondary-регистре. Дубликаты `handle` в `items` допустимы.

Закрытие или передача handle'а, на котором висит активный
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
IPC-буфере вызывающего потока (буферизованный канал удалён). Один кадр несёт
до `IPC_BUFFER_DATA_MAX` байт данных и до `IPC_BUFFER_MAX_CAPS` хэндлов. При
передаче handle'а он удаляется из таблицы отправителя и появляется в таблице
получателя: отправитель теряет доступ к нему.

|     Op | Имя              | Аргументы                  | Возврат                                | Права    |
|-------:|------------------|----------------------------|----------------------------------------|----------|
| `0x23` | `PortCreate` | —                          | `port_h`                           | —        |
| `0x24` | `PortSend`   | `handle`, `timeout_ns`     | `0`                                    | `WRITE`  |
| `0x25` | `PortRecv`   | `handle`, `timeout_ns`     | `reply_h` (либо `0`)                   | `READ`   |
| `0x26` | `PortCall`   | `handle`, `timeout_ns`     | `0`                                    | `WRITE`  |
| `0x27` | `PortReply`  | `reply_handle`             | `0`                                    | `WRITE`  |

`PortCreate` создаёт ОДИН port-объект (стороны симметричны: любой handle
на него можно использовать как локальный и передавать копии другим процессам).

`PortSend` блокирующе отправляет кадр из IPC-буфера текущего потока: ждёт,
пока встречный `recv`/`call` его примет. `PortRecv` блокирующе принимает кадр
в IPC-буфер текущего потока; если встречный был `call`, в возврат кладётся handle
одноразового `Reply`-объекта (иначе `0`). `PortCall` — блокирующий
запрос-ответ: кадр уходит из IPC-буфера, а ответ оказывается в нём же. На
`Reply`-handle сервер вызывает `PortReply`, доставляя кадр из своего
IPC-буфера вызывателю; `Reply` одноразов.

**Тайм-аут (`timeout_ns`).** Блокирующие `PortSend`/`PortRecv`/`PortCall`
ограничивают ожидание встречной стороны параметром `timeout_ns` — чтобы
real-time поток не зависал на IPC неограниченно. Кодировка совпадает по духу
с `SignalWaitOne`, но с sentinel'ом бесконечности:

- `PORT_TIMEOUT_INFINITE` (`u64::MAX`) — ждать бессрочно (поведение по
  умолчанию до введения тайм-аутов; настоящая блокировка без записи в
  sleeper-heap);
- `0` (`PORT_TIMEOUT_POLL`) — не блокироваться: операция либо завершается
  немедленно (встречная сторона уже ждёт), либо возвращает `ShouldWait`;
- иначе — относительный дедлайн в наносекундах.

При истечении тайм-аута (в т.ч. в режиме poll, когда встречной стороны нет)
syscall возвращает `Timeout` (`SYSCALL_RETURN_TIMEOUT = -9`), отличая «истёк
дедлайн» от `ShouldWait`. Для `PortCall` тайм-аут покрывает ВСЮ
операцию (ожидание получателя + ожидание `reply`); если вызывающая сторона уходит по
тайм-ауту, не дождавшись ответа, сервер при попытке `PortReply` получит
`PeerClosed`, а IPC-буфер вызывателя не будет затронут.

**Доставка badge.** На `PortRecv`/`PortCall` ядро дополнительно
записывает в поле `IpcBuffer.badge` получателя значок (badge) port-хендла
ОТПРАВИТЕЛЯ (`0`, если хендл незаклеймён). Так сервер различает клиентов и
соединения, не создавая отдельных port'ов и не опираясь на глобальный
идентификатор объекта: сервер
минтит несколько badged-копий своего port-хендла через `HandleDuplicate`
(каждой — свой badge) и раздаёт их клиентам; клиент шлёт через свою копию, а
сервер на приёме читает её badge. Значок пишется получателю всегда (даже при
пустом теле/без caps); на стороне отправителя поле `badge` не читается. Для
`call` значок вызывателя доставляется серверу на recv; ответ (`PortReply`)
badge не несёт.

Содержимое кадра (байты и хэндлы) кодируется в IPC-буфере по адресу, который
возвращает `IpcBufferAddr` (op `0x56`). Wire-формат типизированного IPC поверх
этого транспорта описан в [ipc.md](ipc.md); он не зависит от того, что underlying
доставка теперь синхронна.

Пример: клиент шлёт запрос сервису и ждёт ответ.

```rust
let ipc = ipc_buffer_addr()? as *mut IpcBuffer;
// уложить кадр запроса в IPC-буфер
write_request_frame(ipc, request);
// блокирующий запрос-ответ: ответ окажется в том же IPC-буфере
port_call(service_h, PORT_TIMEOUT_INFINITE)?;
let response = read_response_frame(ipc);
```

## Handle

Handle-вызовы управляют временем жизни handle'ов в таблице текущего
процесса. Закрытие удаляет запись из таблицы; если это была последняя
ссылка на объект, объект освобождается.

|     Op | Имя               | Аргументы                       | Возврат      | Права       |
|-------:|-------------------|---------------------------------|--------------|-------------|
| `0x30` | `HandleClose`     | `handle`                        | `0`          | —           |
| `0x31` | `HandleDuplicate` | `handle`, `new_rights`, `badge` | `new_handle` | `DUPLICATE` |

### Каскадный отзыв при `HandleClose`

`HandleDuplicate` строит граф деривации: копия — производная (ребёнок)
исходного handle'а. Перенос капы через Port двигает handle целиком, граф
деривации едет с ним и переживает межпроцессную передачу.

`HandleClose` (а также смерть таблицы при завершении процесса) **отзывает всё
поддерево производных** закрываемой капы:

- производные, в том числе в **чужих** таблицах, становятся невалидными —
  любая операция над ними возвращает `Revoked`;
- для memory-капы дополнительно **энергично срываются активные маппинги**
  поддерева (PTE снимаются, range возвращается аллокатору), в том числе в
  чужом адресном пространстве. Маппинг живёт ровно столько, сколько
  авторизовавшая его капа: `HandleClose` снимает и собственные маппинги
  владельца.

Каскад идёт **строго вниз** по графу: сиблинги и предки закрываемой капы
доступ сохраняют. Перенос **корневой** капы (не производной) — безвозвратная
передача владения: грантор узла не держит, отзывать некому.

Два режима раздачи памяти (и любого KO) выражаются одним примитивом:

- `transfer` корня — handover (отозвать нельзя);
- `duplicate` → `transfer` производной — отзываемая «аренда»: закрыл/умер
  грантор → производная у получателя отозвана (`Revoked`).

Гранулярность по клиентам: грантор держит на каждого клиента отдельный
промежуточный узел (`duplicate` под клиента, передать клиенту уже его
производную). Закрыл узел клиента — отвалился только он; закрыл корень —
отвалились все.

Закрытие уже отозванного слота допустимо: освобождает слот, капа не
«застревает».

`new_rights` берётся из нижних 32 бит `arg1` (неизвестные биты отбрасываются) и
обязан быть подмножеством прав исходного хендла. `badge` (полные 64 бита `arg2`,
`0` = без значка) применяется по семантике **set-once** (монотонность, как
badges в seL4):

- источник НЕ заклеймён (`badge == 0`) и `arg2 != 0` → новый хендл заклеймён
  значком `arg2`;
- источник НЕ заклеймён и `arg2 == 0` → новый хендл без значка;
- источник заклеймён и `arg2 == 0` → новый хендл НАСЛЕДУЕТ значок источника;
- источник заклеймён и `arg2 != 0` → попытка переклеймить, ошибка `BadHandle`.

Badge — свойство ХЕНДЛА, а не объекта: разные копии одного и того же port'а
несут разные значки. На приёме сообщения ядро доставляет badge отправителя
получателю (см. [Port](#port)).

Пример: передать в дочерний процесс только право ожидания (без значка).

```rust
let wait_only = handle_duplicate(
    process_h,
    Rights::READ | Rights::TRANSFER,
    0, // без значка
)?;
handle_close(process_h)?;

// Сервер минтит badged-копию port'а под конкретного клиента:
let client_ep = handle_duplicate(port_h, Rights::WRITE, CLIENT_BADGE)?;
```

## Process

Process-вызовы создают процесс, возвращают handle на текущий процесс и
позволяют наблюдать или принудительно завершать процесс по handle.

|     Op | Имя                | Аргументы                                                                                    | Возврат               | Права                                                                          |
|-------:|--------------------|----------------------------------------------------------------------------------------------|-----------------------|--------------------------------------------------------------------------------|
| `0x40` | `ProcessCreate`    | `name_va`, `name_len`                                                                        | `process_h`           | —                                                                              |
| `0x41` | `ProcessSelf`      | —                                                                                            | `process_h`           | —                                                                              |
| `0x42` | `ProcessLoadImage` | `process_h`, `desc_va`, `desc_len`                                                           | `0`                   | `WRITE`, `WRITE`/`READ`/`EXECUTE` на memory-handle'ах сегментов |
| `0x43` | `ProcessExitCode`  | `process_h`                                                                                  | `exit_code` как `u32` | `READ`                                                                      |
| `0x44` | `ProcessTerminate` | `process_h`, `exit_code`                                                                     | `0`                   | `WRITE`                                                               |
| `0x45` | `ProcessStart`     | `process_h`, `entry_pc`, `user_sp`, `arg`, `priority \| (handles_count << 32)`, `handles_va` | `thread_h`            | `WRITE`; `TRANSFER` на bootstrap-handle'ах                            |
| `0x46` | `ProcessTerminationSignal` | `process_h`                                                                          | `signal_h` (read-only)| `READ`                                                               |
| `0x47` | `ProcessResourceSelf` | —                                                                                        | `resource_h`          | —                                                                              |

`ProcessCreate` копирует имя процесса из user-памяти, требует
непустой корректный UTF-8 и ограничение `1 <= name_len <= 64` байт.

`ProcessLoadImage` загружает user-образ в уже созданный, но ещё не
запущенный процесс. `desc_len` обязан быть ровно `56`
([`USER_IMAGE_DESC_SIZE`](../kernel/syscall/src/spawn_abi.rs)), а
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
([`USER_SEGMENT_SIZE`](../kernel/syscall/src/spawn_abi.rs)):

| Offset | Поле            | Тип   | Описание                                             |
|-------:|-----------------|-------|------------------------------------------------------|
|    `0` | `region_handle` | `u32` | handle memory-региона в таблице loader-процесса      |
|    `4` | `flags`         | `u32` | `0 = RW`, `1 = RO`, `2 = RX`                         |
|    `8` | `va_base`       | `u64` | база mapping'а в child AS                            |
|   `16` | `mapped_size`   | `u64` | размер mapping'а; должен совпасть с размером региона |
|   `24` | `reserved`      | `u64` | обязано быть `0`                                     |

Для каждого сегмента loader обязан передать `region_handle` с правом
`WRITE` и правами доступа, совместимыми с `flags`: `READ|WRITE` для `RW`,
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
соответственно. Стартуемый процесс наследует метеринг-`Resource`
вызывающего (caller'а). Как и `ProcessLoadImage`, `ProcessStart` работает только на fresh
child-процессе с уже загруженным образом, нулевым числом потоков и пустой
child handle-таблице; иначе возвращает `WrongType`.

`ProcessResourceSelf` ставит свежий handle на метеринг-`Resource` текущего
процесса (с дефолтными правами, включая `WRITE`) — из его бюджета списываются
`MemoryAllocate`/`MemoryCreateVirtual`/`MemoryCreatePhysical`. `WrongType`,
если метеринг-ресурс не задан.

`ProcessTerminate` не используется для self-exit: handle на текущий
процесс возвращает `AccessDenied` даже при `WRITE`. Собственное
завершение идёт через `ThreadExit`; последний поток процесса помечает
процесс завершённым.

`ProcessTerminationSignal` возвращает handle на ленивый bound-`Signal`
процесса (бит `SIGNALED` = «завершён»), материализуя его при первом вызове;
если процесс уже завершён, `Signal` сразу несёт `SIGNALED`. Выданный handle
read-only (`READ`/`DUPLICATE`/`TRANSFER`, без `WRITE`): наблюдатель не может
подделать термнинацию.

Типичный сценарий user-spawn состоит из трёх шагов: `ProcessCreate`,
затем `ProcessLoadImage`, затем `ProcessStart`. Между `LoadImage` и
`Start` никакой поток в child-процессе ещё не исполняется.

Пример: передать supervisor'у право дождаться завершения текущего процесса.

```rust
// --- сторона процесса (report_h - port к supervisor'у) ---
let process_h = process_self()?;
let wait_h = handle_duplicate(
    process_h,
    Rights::READ | Rights::TRANSFER,
    0, // без значка
)?;
handle_close(process_h)?;
// уложить wait_h как cap в IPC-буфер и доставить его supervisor'у
let ipc = ipc_buffer_addr()? as *mut IpcBuffer;
write_cap_frame(ipc, wait_h);
port_send(report_h, PORT_TIMEOUT_INFINITE)?;

// --- сторона supervisor'а (monitor_h - тот же port) ---
port_recv(monitor_h, PORT_TIMEOUT_INFINITE)?;
let monitored_h = read_cap_frame(ipc).expect("peer transferred wait_h");

let term_h = process_termination_signal(monitored_h)?;
let observed = signal_wait_one(term_h, SIGNALED, timeout_ns)?;
if observed & SIGNALED != 0 {
    let code = process_exit_code(monitored_h)?;
}
```

Пример: создать дочерний процесс, загрузить образ и запустить первый поток.

```rust
let code_region_h = memory_create_virtual(
    PAGE_SIZE,
    AccessMask::R | AccessMask::W | AccessMask::X,
)?;
let child_h = process_create(b"child")?;

// собирает UserImageDesc: сегмент code_region_h на CHILD_CODE_VA,
// entry/стек/окно user-VM
let image = create_user_image(code_region_h)?;

process_load_image(child_h, &image)?;
let first_thread_h = process_start(
    child_h,
    CHILD_CODE_VA,
    CHILD_STACK_TOP,
    /* arg */ 0,
    /* priority | (handles_count << 32) */ 1,
    /* handles_va */ 0,
)?;
```

## Thread

Thread-вызовы создают поток в процессе, возвращают handle на текущий
поток и управляют завершением потока.

|     Op | Имя               | Аргументы                                             | Возврат               | Права            |
|-------:|-------------------|-------------------------------------------------------|-----------------------|------------------|
| `0x50` | `ThreadCreate`    | `process_h`, `entry_pc`, `user_sp`, `arg`, `priority` | `thread_h`            | `WRITE` |
| `0x51` | `ThreadSelf`      | —                                                     | `thread_h`            | —                |
| `0x52` | `ThreadExit`      | `exit_code`                                           | не возвращается       | —                |
| `0x53` | `ThreadExitCode`  | `thread_h`                                            | `exit_code` как `u32` | `READ`        |
| `0x54` | `ThreadTerminate` | `thread_h`, `exit_code`                               | `0`                   | `WRITE`  |
| `0x55` | `ThreadTerminationSignal` | `thread_h`                                    | `signal_h` (read-only)| `READ`        |
| `0x56` | `IpcBufferAddr`           | —                                             | `ipc_buffer_va`       | —             |

`ThreadTerminate` также не используется для self-exit: handle на текущий
поток возвращает `AccessDenied` даже при `WRITE`. Для завершения
текущего потока вызывается `ThreadExit`.

`ThreadTerminationSignal` симметричен `ProcessTerminationSignal`: возвращает
handle на ленивый bound-`Signal` потока (бит `SIGNALED` = «завершён»).

`IpcBufferAddr` возвращает user-VA per-thread IPC-буфера текущего потока; для
kernel-потока без буфера — `-(SyscallError)`. Адрес используется как указатель
на [`IpcBuffer`] перед каждым port-вызовом (см. секцию «Port»).

`exit_code` берётся из младших 32 бит аргумента и читается как `i32`.

Пример: создать worker-поток и дождаться его exit code.

```rust
fn worker_entry() -> ! {
    thread_exit(/* exit_code */ 0)
}

let process_h = process_self()?;
let thread_h = thread_create(process_h, worker_entry as u64, worker_sp, 0, priority)?;

let term_h = thread_termination_signal(thread_h)?;
let observed = signal_wait_one(term_h, SIGNALED, timeout_ns)?;
if observed & SIGNALED != 0 {
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
  port или задублировать с уменьшенными правами.

|     Op | Имя                    | Аргументы                                       | Возврат                                                       | Права                                                                        |
|-------:|------------------------|-------------------------------------------------|---------------------------------------------------------------|------------------------------------------------------------------------------|
| `0x60` | `MemoryCreateVirtual`  | `resource_h`, `size_bytes`, `access_mask`       | `region_h`                                                    | `WRITE` на `Resource`; расходует `size/PAGE` бюджета (`ResourceExhausted` при нехватке), возврат на дропе региона |
| `0x61` | `MemoryCreatePhysical` | `resource_h`, `pa`, `size_bytes`, `access_mask` | `region_h`                                                    | `WRITE` на `Resource`; диапазон и доступ должны укладываться в ресурс; расходует бюджет ресурса (`ResourceExhausted` при нехватке) |
| `0x63` | `MemoryMap`            | `region_h`, `size_bytes`, `flags`               | `va`                                                          | `WRITE` и нужный доступ                                                        |
| `0x64` | `MemoryRemap`          | `va`, `size_bytes`, `flags`                     | `0`                                                           | grant исходного mapping'а                                                    |
| `0x65` | `MemoryAllocate`       | `resource_h`, `size_bytes`, `flags`             | `va`                                                          | `WRITE` на `Resource`; расходует `size/PAGE` бюджета (`ResourceExhausted` при нехватке), возврат на `MemoryFree`/смерти AS |
| `0x66` | `MemoryFree`           | `va`, `size_bytes`                              | `0`                                                           | —                                                                            |
| `0x67` | `MemoryRegionInspect`  | `region_h`                                      | primary=`size_bytes`, secondary=`(kind << 16) \| access_bits` | `READ`                                                                    |

`access_mask`: `R=1`, `W=2`, `X=4`. `flags`: `0=ReadWrite`,
`1=ReadOnly`, `2=ReadExecute`.

`Resource` (KObject, type-tag 5) — это полномочие на минтинг физпамяти
(`MemoryCreatePhysical`), которое дополнительно несёт **бюджет** (счётчик
страниц по 4 KiB). Каждый успешный `MemoryCreatePhysical` атомарно
списывает с бюджета ресурса число затрагиваемых страниц (`size_bytes /
4096`, округление вверх); при нехватке бюджета операция возвращает
`ResourceExhausted` и регион не создаётся. Корневой `Resource` (крупный
PA-диапазон, бюджет `1<<20` страниц) создаётся ядром при старте и выдаётся
bootstrap-процессу как дополнительный initial handle (индекс 1; индекс 0 —
bootstrap-port).

Метеринг-`Resource` разделяется, а не партиционируется: `ProcessStart`
передаёт ребёнку тот же `Resource`, что у caller'а, а `ProcessResourceSelf`
отдаёт на него handle с `WRITE`. Поэтому всё дерево процессов списывает из
одного общего пула; суб-бюджетов на процесс нет, и ребёнок может исчерпать
бюджет родителя и сиблингов.

`MemoryMap` требует, чтобы `size_bytes` совпадал с полным размером
региона; частичный mapping поддиапазона сейчас не поддерживается.

`MemoryMap` привязывает установленный маппинг к капе, через которую он
сделан: маппинг живёт ровно столько, сколько эта капа. `MemoryFree` и
[отзыв капы](#каскадный-отзыв-при-handleclose) идут через **единый** teardown
(снять PTE + вернуть range аллокатору), поэтому при `HandleClose` региона —
своего или у предка по деривации — активные маппинги поддерева срываются
энергично, в том числе кросс-процессно. `MemoryAllocate`-fastpath публичной
капы не имеет: его маппинг снимается только `MemoryFree` или смертью AS.

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
    Rights::WRITE | Rights::READ | Rights::TRANSFER,
    0, // без значка
)?;
// уложить readonly_region_h как cap в IPC-буфер и отправить peer'у
let ipc = ipc_buffer_addr()? as *mut IpcBuffer;
write_cap_frame(ipc, readonly_region_h);
port_send(peer_h, PORT_TIMEOUT_INFINITE)?;
```

## Process Spawning

`ProcessLoadImage` и `ProcessStart` дают userspace полноценную роль
process-manager-а: loader сам пишет байты образа во фреймы регионов через
двойной маппинг и одной syscall просит ядро установить эти регионы в
child AS; вторая syscall атомарно стартует первый поток вместе с
bootstrap-handles.

|     Op | Имя                | Аргументы                                                                                    | Возврат    | Права                                                                            |
|-------:|--------------------|----------------------------------------------------------------------------------------------|------------|----------------------------------------------------------------------------------|
| `0x42` | `ProcessLoadImage` | `process_h`, `desc_va`, `desc_len` (== `56`)                                                 | `0`        | `WRITE` на `process_h`; для каждого региона — `WRITE \| (R/W/X по flags)` |
| `0x45` | `ProcessStart`     | `process_h`, `entry_pc`, `user_sp`, `arg`, `priority \| (handles_count << 32)`, `handles_va` | `thread_h` | `WRITE` на `process_h`; `TRANSFER` на каждом bootstrap-handle           |

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
PortCreate                                   -> bootstrap_h  // делегируется child'у
MemoryCreateVirtual(size, R|W|X access_mask)     -> region_h
MemoryMap(region_h, size, RW)                    -> seg_va  // в loader-AS
copy image bytes -> seg_va                                 // CPU stores
MemoryRemap(seg_va, size, RX)                              // если нужен RX
ProcessCreate("child")                           -> proc_h
build UserImageDescAbi + [UserSegmentAbi; N] на стеке loader-а
ProcessLoadImage(proc_h, desc_va, 56)            -> 0
MemoryFree(seg_va, size)                                   // фреймы остаются за child через Arc<MemoryRegion>
ProcessStart(proc_h, entry_pc, user_sp, arg, prio | (1<<32), &[bootstrap_h])
                                                 -> thread_h
ProcessTerminationSignal(proc_h)                 -> term_h
SignalWaitOne(term_h, SIGNALED, ...)
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
