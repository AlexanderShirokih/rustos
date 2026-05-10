# Lifecycle процессов и потоков

Этот документ описывает модель управления процессами и потоками через
kernel-objects: какие `KObject`-варианты задействованы, какой набор сигналов
поднимается, как пользователь обращается к ним по handle и какой набор
syscall'ов закрывает базовые сценарии.

> Загрузка статической части user-AS — сегменты образа и user-стек —
> описана в [`docs/architecture.md`](architecture.md) (`Per-process AddressSpace`).
> Динамические регионы пользовательской памяти — в
> [`docs/user-memory.md`](user-memory.md).
> Здесь идёт речь только о lifecycle KO: создание, наблюдение завершения,
> чтение exit-кода, принудительное завершение.

## Kernel-objects

Process и Thread представлены отдельными `KObject`-вариантами:

| Вариант             | Сигналы              | Хранит                       |
|---------------------|----------------------|------------------------------|
| `KObject::Process`  | `PROCESS_TERMINATED` | `exit_code: i32`             |
| `KObject::Thread`   | `THREAD_TERMINATED`  | `exit_code: i32`             |

Сигналы поднимаются ровно один раз: первый `signal_terminated` фиксирует
`exit_code` (Release) перед `signals.signal(...)`, что гарантирует
видимость кода тому, кто увидел сигнал через `object_wait_one` и затем
прочитал `exit_code` (Acquire).

`Rights::SIGNAL` для `Process`/`Thread` пользователю **не выдаётся**:
`PROCESS_TERMINATED`/`THREAD_TERMINATED` поднимает только ядро в момент
завершения. Дублирование handle'а с `Rights::SIGNAL` отвергается с
`AccessDenied`.

## Связь со scheduler

`scheduler::Process` и `scheduler::Thread` владеют `Arc<ProcessObject>` и
`Arc<ThreadObject>` соответственно; сильные ссылки переживают запись в
`ProcessTable`/`ThreadTable` через держателей handle'ов. Это означает,
что наблюдатель, дождавшийся `PROCESS_TERMINATED`, может прочитать
`exit_code` процесса даже после того, как scheduler уже удалил его
запись и освободил адресное пространство.

`UserProcessLaunchInfo`, возвращаемый при spawn'е первого user-процесса,
содержит `Arc<ProcessObject>` и `Arc<ThreadObject>` свежесозданных
сущностей — kernelspace bootstrap наблюдает завершение init-процесса
без прохода по handle-table.

## Syscall ABI

Номера операций сгруппированы по высокому ниблу (см. `numbers.rs`):

| Op                  | Hex   | Аргументы                                                    | Возврат                    | Право на handle    |
|---------------------|-------|--------------------------------------------------------------|----------------------------|--------------------|
| `ProcessCreate`     | 0x40  | `name_va`, `name_len`                                        | `HandleId` нового процесса | —                  |
| `ProcessSelf`       | 0x41  | —                                                            | `HandleId` своего процесса | —                  |
| `ProcessExitCode`   | 0x43  | `handle`                                                     | `i32` exit-код             | `INSPECT`          |
| `ProcessTerminate`  | 0x44  | `handle`, `exit_code`                                        | `0`                        | `MANAGE_PROCESS`   |
| `ThreadCreate`      | 0x50  | `process_handle`, `entry_pc`, `user_sp`, `arg`, `priority`   | `HandleId` нового потока   | `MANAGE_PROCESS`   |
| `ThreadSelf`        | 0x51  | —                                                            | `HandleId` своего потока   | —                  |
| `ThreadExit`        | 0x52  | `exit_code`                                                  | не возвращается            | —                  |
| `ThreadExitCode`    | 0x53  | `handle`                                                     | `i32` exit-код             | `INSPECT`          |
| `ThreadTerminate`   | 0x54  | `handle`, `exit_code`                                        | `0`                        | `MANAGE_THREAD`    |

`0x42` зарезервирован под будущие операции класса Process; `0x01`
(старый `ThreadExit`) тоже зарезервирован — попытка вызова возвращает
`BadSyscall`.

`exit_code` передаётся в нижних 32 битах `u64`-аргумента: верхние биты
игнорируются, младшие интерпретируются как `i32` two's complement.

## Стартовые права

`Rights::defaults_for` выдаёт следующие наборы при создании handle'а на
свежий KO (`ProcessCreate` / `ThreadCreate` / `ProcessSelf` / `ThreadSelf`):

| KO       | Стартовые права                                                          |
|----------|--------------------------------------------------------------------------|
| Process  | `WAIT \| INSPECT \| MANAGE_PROCESS \| DUPLICATE \| TRANSFER`             |
| Thread   | `WAIT \| INSPECT \| MANAGE_THREAD \| DUPLICATE \| TRANSFER`              |

`MANAGE_PROCESS` на handle'е процесса позволяет создавать в нём потоки
через `ThreadCreate` и завершать его через `ProcessTerminate`.
`MANAGE_THREAD` на handle'е потока — завершать его через `ThreadTerminate`
(если поток не свой, см. ниже).

## Self-terminate через handle запрещён

`ThreadTerminate(self_handle, code)` и `ProcessTerminate(self_process_handle, code)`
отвергаются с `AccessDenied`. `terminate_thread` / `terminate_process` под
scheduler-lock'ом не выполняют context switch: возврат в диспетчер с уже
завершённым current-потоком привёл бы к записи кода возврата в
терминированный контекст. Путь self-exit — только `ThreadExit(code)`
(op `0x52`), который проваливается через trampoline на следующий
runnable. Завершение последнего потока процесса автоматически поднимает
`PROCESS_TERMINATED` через ту же ветку.

## Паттерн "wait на завершение"

```text
let h = process_create("child")?;
// ... передать h в дочерний или дать ему стартовать через thread_create ...
let observed = object_wait_one(h, PROCESS_TERMINATED, timeout_ns)?;
let code = process_exit_code(h)?;
```

`object_wait_one` принимает любую маску, любая часть которой пересеклась
с уже поднятыми сигналами — fast-path возвращает текущие биты без
парковки. Если сигнал не поднят, поток парковатся в KO-wait через
`block_current_until` и просыпается на `signal_terminated` или
`Timeout`.

Симметричный сценарий для потоков:

```text
let t = thread_create(process_handle, entry_pc, user_sp, arg, priority)?;
object_wait_one(t, THREAD_TERMINATED, None)?; // бессрочный wait
let code = thread_exit_code(t)?;
```

`exit_code` читается через `ProcessExitCode`/`ThreadExitCode`: до подъёма
`*_TERMINATED` они возвращают `0` (поле `exit_code` инициализируется
нулём при создании KO). Наблюдатель, увидевший сигнал и затем
прочитавший код, гарантированно получит финальное значение благодаря
паре Release/Acquire в `signal_terminated` / `exit_code`.

## Освобождение ресурсов

При завершении последнего потока процесса scheduler:

1. Поднимает `THREAD_TERMINATED` на завершающемся потоке.
2. Декрементирует `thread_count` процесса.
3. На нуле — поднимает `PROCESS_TERMINATED` и помечает процесс к
   удалению из `ProcessTable` через `pending_process_removals`.
4. На следующем `switch_to_next` (когда умирающий поток уже не
   `current` ни на одном CPU) запись процесса удаляется. Drop
   последнего `Arc<AddressSpace>` возвращает фреймы mapper'а.

Держатели handle'ов на `ProcessObject`/`ThreadObject` переживают
удаление scheduler-записей: KO живёт пока жив хотя бы один `Arc`.
`exit_code` остаётся доступным через `ProcessExitCode` /
`ThreadExitCode` сколько угодно долго после фактического завершения.
