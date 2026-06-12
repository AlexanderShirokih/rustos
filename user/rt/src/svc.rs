//! Inline-asm реализация svc-обёрток.

use core::arch::asm;

use userland_abi::SyscallOp;

// svc-immediate обязан быть литералом; привязываем его к каноничному ABI-enum.
const _: () = assert!(SyscallOp::ObjectWaitOne as u16 == 0x11);
const _: () = assert!(SyscallOp::ChannelCreate as u16 == 0x20);
const _: () = assert!(SyscallOp::ChannelWrite as u16 == 0x21);
const _: () = assert!(SyscallOp::ChannelRead as u16 == 0x22);
const _: () = assert!(SyscallOp::HandleClose as u16 == 0x30);
const _: () = assert!(SyscallOp::ProcessCreate as u16 == 0x40);
const _: () = assert!(SyscallOp::ProcessSelf as u16 == 0x41);
const _: () = assert!(SyscallOp::ProcessLoadImage as u16 == 0x42);
const _: () = assert!(SyscallOp::ProcessExitCode as u16 == 0x43);
const _: () = assert!(SyscallOp::ProcessStart as u16 == 0x45);
const _: () = assert!(SyscallOp::ThreadSelf as u16 == 0x51);
const _: () = assert!(SyscallOp::ThreadExit as u16 == 0x52);
const _: () = assert!(SyscallOp::MemoryCreateVirtual as u16 == 0x60);
const _: () = assert!(SyscallOp::MemoryMap as u16 == 0x63);
const _: () = assert!(SyscallOp::MemoryRemap as u16 == 0x64);
const _: () = assert!(SyscallOp::MemoryAllocate as u16 == 0x65);
const _: () = assert!(SyscallOp::MemoryRegionInspect as u16 == 0x67);
const _: () = assert!(SyscallOp::MailboxCreate as u16 == 0x70);
const _: () = assert!(SyscallOp::MailboxQueue as u16 == 0x71);
const _: () = assert!(SyscallOp::MailboxWait as u16 == 0x72);

/// Ждёт сигналы `signals` на KO `handle`; `timeout_ns == 0` - non-blocking
/// poll. Возврат: observed-маска (>=0) либо `-(SyscallError)`.
pub fn object_wait_one(handle: usize, signals: u32, timeout_ns: u64) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции (ESR.ISS), аргументы лежат
    // в x0..x2: handle, маска сигналов, timeout_ns; память ядру не
    // передаётся. x0 на выходе: observed-маска (>=0) либо -(SyscallError).
    unsafe {
        asm!(
            "svc #0x11", // SyscallOp::ObjectWaitOne = 0x11
            in("x0") handle as u64,
            in("x1") u64::from(signals),
            in("x2") timeout_ns,
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Пишет `bytes` одним сообщением (без handle'ов) в парный endpoint канала
/// `handle`. Возврат: 0 либо `-(SyscallError)`.
pub fn channel_write(handle: usize, bytes: &[u8]) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции (ESR.ISS), аргументы лежат
    // в x0..x4: handle, bytes_va, bytes_len, handles_va, handles_count.
    // Буфер `bytes` живёт у caller'а (замаплен user_rw), ядро читает его по
    // x1; отсутствие `nomem` не даёт переупорядочить запись буфера за svc.
    unsafe {
        asm!(
            "svc #0x21", // SyscallOp::ChannelWrite = 0x21
            in("x0") handle as u64,
            in("x1") bytes.as_ptr() as u64,
            in("x2") bytes.len() as u64,
            in("x3") 0_u64,
            in("x4") 0_u64,
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Создаёт пару endpoint'ов канала в текущей handle-table. Возврат:
/// `(left, right)`; `left` знаковый (`-(SyscallError)` при ошибке),
/// `right` валиден только при `left >= 0`.
pub fn channel_create() -> (i64, u64) {
    let left: i64;
    let right: u64;
    // SAFETY: svc-immediate несёт номер операции (ESR.ISS), аргументов нет;
    // ядро пишет left-handle в x0 (знаковый), right-handle - в x1.
    unsafe {
        asm!(
            "svc #0x20", // SyscallOp::ChannelCreate = 0x20
            lateout("x0") left,
            lateout("x1") right,
            options(nostack),
        );
    }
    (left, right)
}

/// Достаёт одно сообщение (без handle'ов) из inbound-очереди канала `handle`
/// в `bytes`. Возврат: `bytes_len | (handles_count << 32)` либо
/// `-(SyscallError)`.
pub fn channel_read(handle: usize, bytes: &mut [u8]) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы лежат в x0..x4:
    // handle, bytes_va, bytes_cap, handles_va, handles_cap. Ядро пишет payload
    // по x1 до возврата из svc; буфер живёт у caller'а.
    unsafe {
        asm!(
            "svc #0x22", // SyscallOp::ChannelRead = 0x22
            in("x0") handle as u64,
            in("x1") bytes.as_mut_ptr() as u64,
            in("x2") bytes.len() as u64,
            in("x3") 0_u64,
            in("x4") 0_u64,
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Изымает `handle` из текущей таблицы и закрывает его. Возврат: 0 либо
/// `-(SyscallError)`.
pub fn handle_close(handle: usize) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, x0 - handle; память ядру
    // не передаётся.
    unsafe {
        asm!(
            "svc #0x30", // SyscallOp::HandleClose = 0x30
            in("x0") handle as u64,
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Создаёт пустой user-процесс с именем `name` (UTF-8). Возврат: handle
/// на `ProcessObject` либо `-(SyscallError)`.
pub fn process_create(name: &[u8]) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x1:
    // name_va, name_len; ядро читает имя по x0 до возврата из svc.
    unsafe {
        asm!(
            "svc #0x40", // SyscallOp::ProcessCreate = 0x40
            in("x0") name.as_ptr() as u64,
            in("x1") name.len() as u64,
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Возвращает handle на собственный `ProcessObject` либо `-(SyscallError)`.
pub fn process_self() -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументов нет; x0 на
    // выходе - handle либо -(SyscallError).
    unsafe {
        asm!(
            "svc #0x41", // SyscallOp::ProcessSelf = 0x41
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Загружает образ в процесс `handle` из сериализованного
/// `UserImageDescAbi` в `desc`. Возврат: 0 либо `-(SyscallError)`.
pub fn process_load_image(handle: usize, desc: &[u8]) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x2:
    // process_handle, desc_va, desc_len; ядро копирует дескриптор и массив
    // сегментов из user-памяти до возврата из svc.
    unsafe {
        asm!(
            "svc #0x42", // SyscallOp::ProcessLoadImage = 0x42
            in("x0") handle as u64,
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
pub fn process_exit_code(handle: usize) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, x0 - handle; память ядру
    // не передаётся.
    unsafe {
        asm!(
            "svc #0x43", // SyscallOp::ProcessExitCode = 0x43
            in("x0") handle as u64,
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
    handle: usize,
    entry_pc: u64,
    user_sp: u64,
    arg: u64,
    priority_and_count: u64,
    handles_va: u64,
) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x5; при
    // handles_count > 0 ядро читает массив handle'ов по x5 до возврата из svc.
    unsafe {
        asm!(
            "svc #0x45", // SyscallOp::ProcessStart = 0x45
            in("x0") handle as u64,
            in("x1") entry_pc,
            in("x2") user_sp,
            in("x3") arg,
            in("x4") priority_and_count,
            in("x5") handles_va,
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Возвращает handle на собственный `ThreadObject` либо `-(SyscallError)`.
pub fn thread_self() -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументов нет; x0 на
    // выходе - handle либо -(SyscallError).
    unsafe {
        asm!(
            "svc #0x51", // SyscallOp::ThreadSelf = 0x51
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Создаёт Memory-регион с Virtual backing: `size_bytes`, `access_mask`
/// (биты R/W/X). Возврат: region handle либо `-(SyscallError)`.
pub fn memory_create_virtual(size_bytes: u64, access_mask: u64) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x1;
    // память ядру не передаётся.
    unsafe {
        asm!(
            "svc #0x60", // SyscallOp::MemoryCreateVirtual = 0x60
            in("x0") size_bytes,
            in("x1") access_mask,
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Маппит регион `handle` в текущий user-AS: `flags_raw` - `UserMemFlags`.
/// Возврат: базовый VA либо `-(SyscallError)`.
pub fn memory_map(handle: usize, size_bytes: u64, flags_raw: u64) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x2;
    // память ядру не передаётся.
    unsafe {
        asm!(
            "svc #0x63", // SyscallOp::MemoryMap = 0x63
            in("x0") handle as u64,
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
            "svc #0x64", // SyscallOp::MemoryRemap = 0x64
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
            "svc #0x65", // SyscallOp::MemoryAllocate = 0x65
            in("x0") size_bytes,
            in("x1") flags_raw,
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Инспектирует Memory-регион `handle`. Возврат: `(size_bytes,
/// (kind_tag << 16) | access_bits)`; первый элемент знаковый
/// (`-(SyscallError)` при ошибке), второй валиден только при успехе.
pub fn memory_region_inspect(handle: usize) -> (i64, u64) {
    let primary: i64;
    let secondary: u64;
    // SAFETY: svc-immediate несёт номер операции, x0 - handle; ядро пишет
    // size_bytes в x0, kind|access - в x1.
    unsafe {
        asm!(
            "svc #0x67", // SyscallOp::MemoryRegionInspect = 0x67
            in("x0") handle as u64,
            lateout("x0") primary,
            lateout("x1") secondary,
            options(nostack),
        );
    }
    (primary, secondary)
}

/// Создаёт пустой Mailbox в текущей handle-table. Возврат: handle либо
/// `-(SyscallError)`.
pub fn mailbox_create() -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументов нет; x0 на
    // выходе - handle либо -(SyscallError).
    unsafe {
        asm!(
            "svc #0x70", // SyscallOp::MailboxCreate = 0x70
            lateout("x0") ret,
            options(nostack),
        );
    }
    ret
}

/// Кладёт `packet` (ровно `MAILBOX_PACKET_SIZE` байт) в очередь mailbox'а
/// `handle`. Возврат: 0 либо `-(SyscallError)`.
pub fn mailbox_queue(handle: usize, packet: &[u8]) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x2:
    // handle, packet_va, packet_len; ядро читает пакет по x1 до возврата
    // из svc.
    unsafe {
        asm!(
            "svc #0x71", // SyscallOp::MailboxQueue = 0x71
            in("x0") handle as u64,
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
pub fn mailbox_wait(handle: usize, timeout_ns: u64, packet: &mut [u8]) -> i64 {
    let ret: i64;
    // SAFETY: svc-immediate несёт номер операции, аргументы в x0..x3:
    // handle, timeout_ns, packet_va, packet_cap; ядро пишет пакет по x2 до
    // возврата из svc.
    unsafe {
        asm!(
            "svc #0x72", // SyscallOp::MailboxWait = 0x72
            in("x0") handle as u64,
            in("x1") timeout_ns,
            in("x2") packet.as_mut_ptr() as u64,
            in("x3") packet.len() as u64,
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
            "svc #0x52", // SyscallOp::ThreadExit = 0x52
            in("x0") code,
            options(noreturn, nostack),
        );
    }
}
