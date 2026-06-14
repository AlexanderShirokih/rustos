//! Inline-asm реализация svc-обёрток.

use core::arch::asm;

use syscall::{Handle, SyscallOp, WaitItem};

/// Ждёт сигналы `signals` на KO `handle`; `timeout_ns == 0` - non-blocking
/// poll. Возврат: observed-маска (>=0) либо `-(SyscallError)`.
pub fn object_wait_one(handle: Handle, signals: u32, timeout_ns: u64) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции (ESR.ISS), аргументы лежат
    // в x0..x2: handle, маска сигналов, timeout_ns; память ядру не
    // передаётся. x0 на выходе: observed-маска (>=0) либо -(SyscallError).
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::ObjectWaitOne as u16,
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
pub fn object_signal(handle: Handle, set: u32, clear: u32, count: u32) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции (ESR.ISS), аргументы лежат
    // в x0..x3: handle, set, clear, count; память ядру не передаётся.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::ObjectSignal as u16,
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
pub fn object_wait_many(items: &[WaitItem], timeout_ns: u64) -> (i64, u64) {
    let observed: i64;
    let index: u64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x2:
    // items_va, count, timeout_ns; ядро читает массив записей по x0
    // (wire-layout WaitItem) до возврата из svc. На выходе x0 -
    // observed-маска, x1 - индекс.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::ObjectWaitMany as u16,
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

/// Создаёт пустой Event в текущей handle-table. Возврат: handle либо
/// `-(SyscallError)`.
pub fn event_create() -> Result<Handle, i64> {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументов нет; x0 на
    // выходе - handle либо -(SyscallError).
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::EventCreate as u16,
            lateout("x0") ret,
            options(nostack),
        );
    }
    Handle::from_syscall_return(ret)
}

/// Пишет `bytes` и `handles` (передаются с потерей у отправителя) одним
/// сообщением в парный endpoint канала `handle`. Возврат: 0 либо
/// `-(SyscallError)`.
pub fn channel_write(handle: Handle, bytes: &[u8], handles: &[Handle]) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции (ESR.ISS), аргументы лежат
    // в x0..x4: handle, bytes_va, bytes_len, handles_va, handles_count.
    // `handles` - repr(transparent) над `u32`, поэтому срез по x3 - это
    // массив HandleId, который ждёт ядро. Буферы `bytes`/`handles` живут у
    // caller'а (замаплены user_rw), ядро читает их по x1/x3; отсутствие
    // `nomem` не даёт переупорядочить запись буферов за svc.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::ChannelWrite as u16,
            in("x0") u64::from(handle.raw()),
            in("x1") bytes.as_ptr() as u64,
            in("x2") bytes.len() as u64,
            in("x3") handles.as_ptr() as u64,
            in("x4") handles.len() as u64,
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Создаёт пару endpoint'ов канала в текущей handle-table. Возврат:
/// `(left, right)` либо `-(SyscallError)` из x0.
pub fn channel_create() -> Result<(Handle, Handle), i64> {
    let left: i64;
    let right: u64;
    // SAFETY: svc-immediate несёт номер операции (ESR.ISS), аргументов нет;
    // ядро пишет left-handle в x0 (знаковый), right-handle - в x1.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::ChannelCreate as u16,
            lateout("x0") left,
            lateout("x1") right,
            options(nostack),
        );
    }
    let left = Handle::from_syscall_return(left)?;
    let right = Handle::new(right as u32).ok_or(0_i64)?;
    Ok((left, right))
}

/// Достаёт одно сообщение из inbound-очереди канала `handle` в `bytes` и
/// `handles` (принятые handle, по одному на слот). Возврат: `bytes_len |
/// (handles_count << 32)` либо `-(SyscallError)`.
pub fn channel_read(handle: Handle, bytes: &mut [u8], handles: &mut [Option<Handle>]) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы лежат в x0..x4:
    // handle, bytes_va, bytes_cap, handles_va, handles_cap. `Option<Handle>`
    // имеет layout `u32` (niche `0 == None`), поэтому ядро пишет ненулевые
    // HandleId по x3 в первые handles_count слотов (валидный `Some`),
    // остальные остаются `None`. Payload идёт по x1; буферы живут у caller'а.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::ChannelRead as u16,
            in("x0") u64::from(handle.raw()),
            in("x1") bytes.as_mut_ptr() as u64,
            in("x2") bytes.len() as u64,
            in("x3") handles.as_mut_ptr() as u64,
            in("x4") handles.len() as u64,
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
/// неизвестные биты отбрасываются). Возврат: новый handle либо
/// `-(SyscallError)`.
pub fn handle_duplicate(handle: Handle, new_rights: u32) -> Result<Handle, i64> {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x1:
    // handle, new_rights (нижние 32 бита); память ядру не передаётся.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::HandleDuplicate as u16,
            in("x0") u64::from(handle.raw()),
            in("x1") u64::from(new_rights),
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

/// Завершает процесс `handle` с кодом `exit_code` (нижние 32 бита):
/// всем потокам поднимает `THREAD_TERMINATED`, после декремента до нуля -
/// `PROCESS_TERMINATED`. Возврат: 0 либо `-(SyscallError)`.
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

/// Создаёт Memory-регион с Virtual backing: `size_bytes`, `access_mask`
/// (биты R/W/X). Возврат: region handle либо `-(SyscallError)`.
pub fn memory_create_virtual(size_bytes: u64, access_mask: u64) -> Result<Handle, i64> {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x1;
    // память ядру не передаётся.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::MemoryCreateVirtual as u16,
            in("x0") size_bytes,
            in("x1") access_mask,
            lateout("x0") ret,
            options(nostack),
        );
    }
    Handle::from_syscall_return(ret)
}

/// Создаёт Memory-регион с Physical backing: `resource` - handle на
/// `PhysicalResource`, `pa` - физический адрес (page-aligned), `size_bytes`,
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

/// Выделяет анонимный регион и маппит его в свободный VA: `flags_raw` -
/// `UserMemFlags`. Возврат: базовый VA либо `-(SyscallError)`.
pub fn memory_allocate(size_bytes: u64, flags_raw: u64) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x1;
    // память ядру не передаётся.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::MemoryAllocate as u16,
            in("x0") size_bytes,
            in("x1") flags_raw,
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

/// Создаёт пустой Mailbox в текущей handle-table. Возврат: handle либо
/// `-(SyscallError)`.
pub fn mailbox_create() -> Result<Handle, i64> {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументов нет; x0 на
    // выходе - handle либо -(SyscallError).
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::MailboxCreate as u16,
            lateout("x0") ret,
            options(nostack),
        );
    }
    Handle::from_syscall_return(ret)
}

/// Кладёт `packet` (ровно `MAILBOX_PACKET_SIZE` байт) в очередь mailbox'а
/// `handle`. Возврат: 0 либо `-(SyscallError)`.
pub fn mailbox_queue(handle: Handle, packet: &[u8]) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x2:
    // handle, packet_va, packet_len; ядро читает пакет по x1 до возврата
    // из svc.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::MailboxQueue as u16,
            in("x0") u64::from(handle.raw()),
            in("x1") packet.as_ptr() as u64,
            in("x2") packet.len() as u64,
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Ждёт пакет в mailbox'е `handle` (`timeout_ns == 0` - non-blocking poll)
/// и пишет его в `packet`. Возврат: длина пакета либо `-(SyscallError)`.
pub fn mailbox_wait(handle: Handle, timeout_ns: u64, packet: &mut [u8]) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x3:
    // handle, timeout_ns, packet_va, packet_cap; ядро пишет пакет по x2 до
    // возврата из svc.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::MailboxWait as u16,
            in("x0") u64::from(handle.raw()),
            in("x1") timeout_ns,
            in("x2") packet.as_mut_ptr() as u64,
            in("x3") packet.len() as u64,
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Подписывает mailbox `mbox` на сигналы `target`: `key` - идентификатор
/// подписки для последующей отмены, `mask_and_mode = mask | (mode << 32)`
/// (`mode == 0` - Once, `1` - Repeating). Возврат: 0 либо `-(SyscallError)`.
pub fn mailbox_wait_async(mbox: Handle, target: Handle, key: u64, mask_and_mode: u64) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x3:
    // mbox_handle, target_handle, key, mask|(mode<<32); память ядру не
    // передаётся.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::MailboxWaitAsync as u16,
            in("x0") u64::from(mbox.raw()),
            in("x1") u64::from(target.raw()),
            in("x2") key,
            in("x3") mask_and_mode,
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Снимает подписку `mbox` на сигналы `target` с ключом `key`. Идемпотентен:
/// отсутствующая подписка - 0. Возврат: 0 либо `-(SyscallError)`.
pub fn mailbox_cancel(mbox: Handle, target: Handle, key: u64) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x2:
    // mbox_handle, target_handle, key; память ядру не передаётся.
    unsafe {
        asm!(
            "svc #{op}",
            op = const SyscallOp::MailboxCancel as u16,
            in("x0") u64::from(mbox.raw()),
            in("x1") u64::from(target.raw()),
            in("x2") key,
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
