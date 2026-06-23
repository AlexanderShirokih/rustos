//! Inline-asm реализация svc-обёрток.

use core::arch::asm;

use syscall::{Handle, SyscallOp, WaitItem};

/// Ждёт сигналы `signals` на KO `handle`; `timeout_ns == 0` - non-blocking
/// poll. Возврат: observed-маска (>=0) либо `-(SyscallError)`.
pub fn signal_wait_one(handle: Handle, signals: u32, timeout_ns: u64) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции (ESR.ISS), аргументы лежат
    // в x0..x2: handle, маска сигналов, timeout_ns; память ядру не
    // передаётся. x0 на выходе: observed-маска (>=0) либо -(SyscallError).
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::SignalWaitOne as u16,
            in("x0") u64::from(handle.raw()),
            in("x1") u64::from(signals),
            in("x2") timeout_ns,
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Меняет биты сигналов KO `handle`: `set`/`clear` - нижние 32 бита,
/// `count == 0` будит всех пересекающихся waiter'ов, `count == N>0` -
/// не более N в FIFO-порядке. Возврат: 0 либо `-(SyscallError)`.
pub fn signal_set(handle: Handle, set: u32, clear: u32, count: u32) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции (ESR.ISS), аргументы лежат
    // в x0..x3: handle, set, clear, count; память ядру не передаётся.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::SignalSet as u16,
            in("x0") u64::from(handle.raw()),
            in("x1") u64::from(set),
            in("x2") u64::from(clear),
            in("x3") u64::from(count),
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Ждёт сигналы на нескольких KO: `items` - записи `[handle, mask]`,
/// `timeout_ns == 0` - non-blocking poll. Возврат: `(observed, index)` -
/// observed-маска сработавшего KO (>=0 либо `-(SyscallError)`) и его индекс
/// в `items`.
pub fn signal_wait_many(items: &[WaitItem], timeout_ns: u64) -> (i64, u64) {
    let observed: i64;
    let index: u64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x2:
    // items_va, count, timeout_ns; ядро читает массив записей по x0
    // (wire-layout WaitItem) до возврата из svc. На выходе x0 -
    // observed-маска, x1 - индекс.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::SignalWaitMany as u16,
            in("x0") items.as_ptr() as u64,
            in("x1") items.len() as u64,
            in("x2") timeout_ns,
            lateout("x0") observed,
            lateout("x1") index,
            options(nostack),
        );
    }
    (observed, index)
}

/// Создаёт пустой `Signal` в текущей handle-таблице. Возврат: handle
/// либо `-(SyscallError)`.
pub fn signal_create() -> Result<Handle, i64> {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументов нет; x0 на
    // выходе - handle либо -(SyscallError).
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::SignalCreate as u16,
            lateout("x0") ret,
            options(nostack),
        );
    }
    Handle::from_syscall_return(ret)
}

/// Создаёт `Port` (synchronous rendezvous-IPC) в текущей таблице.
/// Возврат: ОДИН port-handle либо `-(SyscallError)`.
pub fn port_create() -> Result<Handle, i64> {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументов нет; x0 на
    // выходе - handle либо -(SyscallError).
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::PortCreate as u16,
            lateout("x0") ret,
            options(nostack),
        );
    }
    Handle::from_syscall_return(ret)
}

/// `send` на port `handle`: блокирующая отправка сообщения из
/// IPC-буфера текущего потока. `timeout_ns`:
/// [`PORT_TIMEOUT_INFINITE`](syscall::PORT_TIMEOUT_INFINITE) - бессрочно,
/// `0` - poll, иначе дедлайн в нс. Возврат: 0,
/// [`SYSCALL_RETURN_TIMEOUT`](syscall::SYSCALL_RETURN_TIMEOUT) при
/// истечении тайм-аута, либо `-(SyscallError)`.
pub fn port_send(handle: Handle, timeout_ns: u64) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, x0 - handle, x1 -
    // timeout_ns; сообщение лежит в per-thread IPC-буфере (ядро читает
    // его само), память по указателю не передаётся.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::PortSend as u16,
            in("x0") u64::from(handle.raw()),
            in("x1") timeout_ns,
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// `recv` на port `handle`: блокирующий приём в IPC-буфер текущего
/// потока. `timeout_ns` - как у [`port_send`]. Возврат: reply handle id
/// (если встречный был `call`, иначе 0),
/// [`SYSCALL_RETURN_TIMEOUT`](syscall::SYSCALL_RETURN_TIMEOUT) при
/// истечении тайм-аута, либо `-(SyscallError)`.
pub fn port_recv(handle: Handle, timeout_ns: u64) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, x0 - handle, x1 -
    // timeout_ns; принятое сообщение ядро пишет в per-thread IPC-буфер
    // текущего потока.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::PortRecv as u16,
            in("x0") u64::from(handle.raw()),
            in("x1") timeout_ns,
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// `call` на port `handle`: блокирующий запрос-ответ. Сообщение из
/// IPC-буфера текущего потока; ответ оказывается там же. `timeout_ns`
/// ограничивает всю операцию (см. [`port_send`]). Возврат: 0,
/// [`SYSCALL_RETURN_TIMEOUT`](syscall::SYSCALL_RETURN_TIMEOUT) при
/// истечении тайм-аута, либо `-(SyscallError)`.
pub fn port_call(handle: Handle, timeout_ns: u64) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, x0 - handle, x1 -
    // timeout_ns; запрос и ответ ходят через per-thread IPC-буфер текущего
    // потока.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::PortCall as u16,
            in("x0") u64::from(handle.raw()),
            in("x1") timeout_ns,
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// `reply` на одноразовый `reply_handle`: доставляет ответ из IPC-буфера
/// сервера вызывателю. Возврат: 0 либо `-(SyscallError)`.
pub fn port_reply(reply_handle: Handle) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, x0 - reply handle; ответ
    // лежит в per-thread IPC-буфере сервера.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::PortReply as u16,
            in("x0") u64::from(reply_handle.raw()),
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Изымает `handle` из текущей таблицы и закрывает его. Возврат: 0 либо
/// `-(SyscallError)`.
pub fn handle_close(handle: Handle) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, x0 - handle; память ядру
    // не передаётся.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::HandleClose as u16,
            in("x0") u64::from(handle.raw()),
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Дублирует `handle` с правами `new_rights` (подмножество исходных,
/// неизвестные биты отбрасываются) и значком `badge` (set-once: `0` - без
/// значка / наследовать). Возврат: новый handle либо `-(SyscallError)`.
///
/// Семантика `badge`: заклеймить можно только незаклеймённый источник;
/// заклеймённый наследует свой значок при `badge == 0`, а попытка
/// переклеймить (`badge != 0` на уже заклеймённом) даёт `BadHandle`.
pub fn handle_duplicate(handle: Handle, new_rights: u32, badge: u64) -> Result<Handle, i64> {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x2:
    // handle, new_rights (нижние 32 бита), badge (полные 64 бита); память
    // ядру не передаётся.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::HandleDuplicate as u16,
            in("x0") u64::from(handle.raw()),
            in("x1") u64::from(new_rights),
            in("x2") badge,
            lateout("x0") ret,
            options(nostack),
        );
    }
    Handle::from_syscall_return(ret)
}

/// Создаёт пустой user-процесс с именем `name` (UTF-8). Возврат: handle
/// на `ProcessObject` либо `-(SyscallError)`.
pub fn process_create(name: &[u8]) -> Result<Handle, i64> {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x1:
    // name_va, name_len; ядро читает имя по x0 до возврата из svc.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::ProcessCreate as u16,
            in("x0") name.as_ptr() as u64,
            in("x1") name.len() as u64,
            lateout("x0") ret,
            options(nostack),
        );
    }
    Handle::from_syscall_return(ret)
}

/// Возвращает handle на собственный `ProcessObject` либо `-(SyscallError)`.
pub fn process_self() -> Result<Handle, i64> {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументов нет; x0 на
    // выходе - handle либо -(SyscallError).
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::ProcessSelf as u16,
            lateout("x0") ret,
            options(nostack),
        );
    }
    Handle::from_syscall_return(ret)
}

/// Возвращает свежий handle на метеринг-`Resource` текущего процесса (права
/// включают `WRITE` для минтинга). Возврат: resource-handle либо
/// `-(SyscallError)` (`WrongType`, если процесс без метеринг-ресурса).
pub fn process_resource_self() -> Result<Handle, i64> {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументов нет; x0 на
    // выходе - handle либо -(SyscallError).
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::ProcessResourceSelf as u16,
            lateout("x0") ret,
            options(nostack),
        );
    }
    Handle::from_syscall_return(ret)
}

/// Загружает образ в процесс `handle` из сериализованного
/// `UserImageDescAbi` в `desc`. Возврат: 0 либо `-(SyscallError)`.
pub fn process_load_image(handle: Handle, desc: &[u8]) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x2:
    // process_handle, desc_va, desc_len; ядро копирует дескриптор и массив
    // сегментов из user-памяти до возврата из svc.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::ProcessLoadImage as u16,
            in("x0") u64::from(handle.raw()),
            in("x1") desc.as_ptr() as u64,
            in("x2") desc.len() as u64,
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Финальный exit-код процесса `handle`. Возврат: exit code
/// (нижние 32 бита, zero-extended) либо `-(SyscallError)`.
pub fn process_exit_code(handle: Handle) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, x0 - handle; память ядру
    // не передаётся.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::ProcessExitCode as u16,
            in("x0") u64::from(handle.raw()),
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Возвращает handle на ленивый bound-`Signal` термнинации процесса `handle`
/// (бит `SIGNALED`). Возврат: signal-handle либо `-(SyscallError)`.
pub fn process_termination_signal(handle: Handle) -> Result<Handle, i64> {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, x0 - handle; память ядру
    // не передаётся. x0 на выходе - handle либо -(SyscallError).
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::ProcessTerminationSignal as u16,
            in("x0") u64::from(handle.raw()),
            lateout("x0") ret,
            options(nostack),
        );
    }
    Handle::from_syscall_return(ret)
}

/// Завершает процесс `handle` с кодом `exit_code` (нижние 32 бита): помечает
/// завершёнными все его потоки, после декремента до нуля - и сам процесс
/// (bound-`Signal`'ы получают `SIGNALED`). Возврат: 0 либо `-(SyscallError)`.
pub fn process_terminate(handle: Handle, exit_code: u64) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x1:
    // handle, exit_code; память ядру не передаётся.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::ProcessTerminate as u16,
            in("x0") u64::from(handle.raw()),
            in("x1") exit_code,
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Стартует первый поток процесса `handle`:
/// `priority_and_count = priority | (handles_count << 32)`, `handles_va` -
/// массив `[u32]` HandleId. Возврат: handle на `ThreadObject` либо
/// `-(SyscallError)`.
pub fn process_start(
    handle: Handle,
    entry_pc: u64,
    user_sp: u64,
    arg: u64,
    priority_and_count: u64,
    handles_va: u64,
) -> Result<Handle, i64> {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x5; при
    // handles_count > 0 ядро читает массив handle'ов по x5 до возврата из svc.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::ProcessStart as u16,
            in("x0") u64::from(handle.raw()),
            in("x1") entry_pc,
            in("x2") user_sp,
            in("x3") arg,
            in("x4") priority_and_count,
            in("x5") handles_va,
            lateout("x0") ret,
            options(nostack),
        );
    }
    Handle::from_syscall_return(ret)
}

/// Создаёт поток в процессе `process`: `entry_pc`, `user_sp`, `arg` (X0
/// первой инструкции), `priority`. Возврат: handle на `ThreadObject` либо
/// `-(SyscallError)`.
pub fn thread_create(
    process: Handle,
    entry_pc: u64,
    user_sp: u64,
    arg: u64,
    priority: u64,
) -> Result<Handle, i64> {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x4:
    // process_handle, entry_pc, user_sp, arg, priority; память ядру не
    // передаётся.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::ThreadCreate as u16,
            in("x0") u64::from(process.raw()),
            in("x1") entry_pc,
            in("x2") user_sp,
            in("x3") arg,
            in("x4") priority,
            lateout("x0") ret,
            options(nostack),
        );
    }
    Handle::from_syscall_return(ret)
}

/// Возвращает handle на собственный `ThreadObject` либо `-(SyscallError)`.
pub fn thread_self() -> Result<Handle, i64> {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументов нет; x0 на
    // выходе - handle либо -(SyscallError).
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::ThreadSelf as u16,
            lateout("x0") ret,
            options(nostack),
        );
    }
    Handle::from_syscall_return(ret)
}

/// Финальный exit-код потока `handle`. Возврат: exit code (нижние 32
/// бита, zero-extended) либо `-(SyscallError)`.
pub fn thread_exit_code(handle: Handle) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, x0 - handle; память ядру
    // не передаётся.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::ThreadExitCode as u16,
            in("x0") u64::from(handle.raw()),
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Возвращает handle на ленивый bound-`Signal` термнинации потока `handle`
/// (бит `SIGNALED`). Возврат: signal-handle либо `-(SyscallError)`.
pub fn thread_termination_signal(handle: Handle) -> Result<Handle, i64> {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, x0 - handle; память ядру
    // не передаётся. x0 на выходе - handle либо -(SyscallError).
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::ThreadTerminationSignal as u16,
            in("x0") u64::from(handle.raw()),
            lateout("x0") ret,
            options(nostack),
        );
    }
    Handle::from_syscall_return(ret)
}

/// Завершает поток `handle` с кодом `exit_code` (нижние 32 бита). Возврат:
/// 0 либо `-(SyscallError)`. Терминирование собственного потока через
/// handle отвергается - для self-exit есть `thread_exit`.
pub fn thread_terminate(handle: Handle, exit_code: u64) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x1:
    // handle, exit_code; память ядру не передаётся.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::ThreadTerminate as u16,
            in("x0") u64::from(handle.raw()),
            in("x1") exit_code,
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Создаёт Memory-регион с Virtual backing: `resource` - handle на
/// метеринг-`Resource` (требует `WRITE`), `size_bytes`, `access_mask`
/// (биты R/W/X). Возврат: region handle либо `-(SyscallError)`.
pub fn memory_create_virtual(
    resource: Handle,
    size_bytes: u64,
    access_mask: u64,
) -> Result<Handle, i64> {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x2;
    // память ядру не передаётся.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::MemoryCreateVirtual as u16,
            in("x0") u64::from(resource.raw()),
            in("x1") size_bytes,
            in("x2") access_mask,
            lateout("x0") ret,
            options(nostack),
        );
    }
    Handle::from_syscall_return(ret)
}

/// Создаёт Memory-регион с Physical backing: `resource` - handle на
/// `Resource`, `pa` - физический адрес (page-aligned), `size_bytes`,
/// `access_mask` (биты R/W/X). Возврат: region handle либо `-(SyscallError)`.
pub fn memory_create_physical(
    resource: Handle,
    pa: u64,
    size_bytes: u64,
    access_mask: u64,
) -> Result<Handle, i64> {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x3:
    // resource_handle, pa, size_bytes, access_mask; память ядру не передаётся.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::MemoryCreatePhysical as u16,
            in("x0") u64::from(resource.raw()),
            in("x1") pa,
            in("x2") size_bytes,
            in("x3") access_mask,
            lateout("x0") ret,
            options(nostack),
        );
    }
    Handle::from_syscall_return(ret)
}

/// Маппит регион `handle` в текущий user-AS: `flags_raw` - `UserMemFlags`.
/// Возврат: базовый VA либо `-(SyscallError)`.
pub fn memory_map(handle: Handle, size_bytes: u64, flags_raw: u64) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x2;
    // память ядру не передаётся.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::MemoryMap as u16,
            in("x0") u64::from(handle.raw()),
            in("x1") size_bytes,
            in("x2") flags_raw,
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Меняет флаги существующего маппинга по `va`: `flags_raw` -
/// `UserMemFlags`. Возврат: 0 либо `-(SyscallError)`.
pub fn memory_remap(va: u64, size_bytes: u64, flags_raw: u64) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x2;
    // память ядру не передаётся.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::MemoryRemap as u16,
            in("x0") va,
            in("x1") size_bytes,
            in("x2") flags_raw,
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Выделяет анонимный регион и маппит его в свободный VA: `resource` - handle
/// на метеринг-`Resource` (требует `WRITE`), `size_bytes`, `flags_raw` -
/// `UserMemFlags`. Возврат: базовый VA либо `-(SyscallError)`.
pub fn memory_allocate(resource: Handle, size_bytes: u64, flags_raw: u64) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x2;
    // память ядру не передаётся.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::MemoryAllocate as u16,
            in("x0") u64::from(resource.raw()),
            in("x1") size_bytes,
            in("x2") flags_raw,
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Снимает маппинг по `va` (page-aligned) длиной `size_bytes` и возвращает
/// регион в free-list. Возврат: 0 либо `-(SyscallError)`.
pub fn memory_free(va: u64, size_bytes: u64) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x1:
    // va, size_bytes; память ядру не передаётся.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::MemoryFree as u16,
            in("x0") va,
            in("x1") size_bytes,
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Инспектирует Memory-регион `handle`. Возврат: `(size_bytes,
/// (kind_tag << 16) | access_bits)`; первый элемент знаковый
/// (`-(SyscallError)` при ошибке), второй валиден только при успехе.
pub fn memory_region_inspect(handle: Handle) -> (i64, u64) {
    let primary: i64;
    let secondary: u64;
    // SAFETY: svc-immediate несёт номер операции, x0 - handle; ядро пишет
    // size_bytes в x0, kind|access - в x1.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::MemoryRegionInspect as u16,
            in("x0") u64::from(handle.raw()),
            lateout("x0") primary,
            lateout("x1") secondary,
            options(nostack),
        );
    }
    (primary, secondary)
}

/// Возвращает user-VA per-thread IPC-буфера текущего потока. Возврат:
/// VA (>0) либо `-(SyscallError)`, если у потока нет буфера.
pub fn ipc_buffer_addr() -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументов нет; x0 на
    // выходе - user-VA буфера (>0) либо -(SyscallError). Память ядру не
    // передаётся.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::IpcBufferAddr as u16,
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Завершает текущий поток с кодом `code`; не возвращается.
pub fn thread_exit(code: u64) -> ! {
    // SAFETY: ThreadExit не возвращается, поэтому asm помечен noreturn и
    // удовлетворяет `-> !`; x0 несёт exit code.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::ThreadExit as u16,
            in("x0") code,
            options(noreturn, nostack),
        )
    }
}
