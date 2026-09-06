//! Inline-asm реализация svc-обёрток.
//!
//! Макросы ставят ловушку: номер операции идёт immediate-полем `svc`, аргументы - в x0 и
//! дальше по порядку, возврат - в x0. Обёртка остаётся типизированной подписью с обоснованием
//! своего `unsafe`.
//!
//! Ловушка существует только под RustOS на aarch64. На любом другом таргете макросы отдают
//! код `BadSyscall`, и крейт собирается для линтов и хостовых тестов зависимых крейтов.
//! Блоки `unsafe` обёрток при этом остаются без unsafe-операции, и предупреждение о них снято
//! на весь модуль.

#![cfg_attr(
    not(all(target_os = "molecular", target_arch = "aarch64")),
    allow(unused_unsafe)
)]

#[cfg(all(target_os = "molecular", target_arch = "aarch64"))]
use core::arch::asm;

trait Argument {
    fn to_register(self) -> u64;
}

impl Argument for u64 {
    fn to_register(self) -> u64 {
        self
    }
}

impl Argument for u32 {
    fn to_register(self) -> u64 {
        u64::from(self)
    }
}

impl Argument for u16 {
    fn to_register(self) -> u64 {
        u64::from(self)
    }
}

impl Argument for Handle {
    fn to_register(self) -> u64 {
        u64::from(self.raw())
    }
}

#[cfg(all(target_os = "molecular", target_arch = "aarch64"))]
macro_rules! svc {
    ($op:expr) => {{
        let ret: i64;
        asm!(
            "svc #{op}",
            op = const $op as u16,
            lateout("x0") ret,
            options(nostack),
        );
        ret
    }};
    ($op:expr, $x0:expr) => {{
        let ret: i64;
        asm!(
            "svc #{op}",
            op = const $op as u16,
            in("x0") Argument::to_register($x0),
            lateout("x0") ret,
            options(nostack),
        );
        ret
    }};
    ($op:expr, $x0:expr, $x1:expr) => {{
        let ret: i64;
        asm!(
            "svc #{op}",
            op = const $op as u16,
            in("x0") Argument::to_register($x0),
            in("x1") Argument::to_register($x1),
            lateout("x0") ret,
            options(nostack),
        );
        ret
    }};
    ($op:expr, $x0:expr, $x1:expr, $x2:expr) => {{
        let ret: i64;
        asm!(
            "svc #{op}",
            op = const $op as u16,
            in("x0") Argument::to_register($x0),
            in("x1") Argument::to_register($x1),
            in("x2") Argument::to_register($x2),
            lateout("x0") ret,
            options(nostack),
        );
        ret
    }};
    ($op:expr, $x0:expr, $x1:expr, $x2:expr, $x3:expr) => {{
        let ret: i64;
        asm!(
            "svc #{op}",
            op = const $op as u16,
            in("x0") Argument::to_register($x0),
            in("x1") Argument::to_register($x1),
            in("x2") Argument::to_register($x2),
            in("x3") Argument::to_register($x3),
            lateout("x0") ret,
            options(nostack),
        );
        ret
    }};
    ($op:expr, $x0:expr, $x1:expr, $x2:expr, $x3:expr, $x4:expr) => {{
        let ret: i64;
        asm!(
            "svc #{op}",
            op = const $op as u16,
            in("x0") Argument::to_register($x0),
            in("x1") Argument::to_register($x1),
            in("x2") Argument::to_register($x2),
            in("x3") Argument::to_register($x3),
            in("x4") Argument::to_register($x4),
            lateout("x0") ret,
            options(nostack),
        );
        ret
    }};
    ($op:expr, $x0:expr, $x1:expr, $x2:expr, $x3:expr, $x4:expr, $x5:expr) => {{
        let ret: i64;
        asm!(
            "svc #{op}",
            op = const $op as u16,
            in("x0") Argument::to_register($x0),
            in("x1") Argument::to_register($x1),
            in("x2") Argument::to_register($x2),
            in("x3") Argument::to_register($x3),
            in("x4") Argument::to_register($x4),
            in("x5") Argument::to_register($x5),
            lateout("x0") ret,
            options(nostack),
        );
        ret
    }};
}

#[cfg(all(target_os = "molecular", target_arch = "aarch64"))]
macro_rules! svc_pair {
    ($op:expr, $x0:expr) => {{
        let primary: i64;
        let secondary: u64;
        asm!(
            "svc #{op}",
            op = const $op as u16,
            in("x0") Argument::to_register($x0),
            lateout("x0") primary,
            lateout("x1") secondary,
            options(nostack),
        );
        (primary, secondary)
    }};
    ($op:expr, $x0:expr, $x1:expr, $x2:expr) => {{
        let primary: i64;
        let secondary: u64;
        asm!(
            "svc #{op}",
            op = const $op as u16,
            in("x0") Argument::to_register($x0),
            in("x1") Argument::to_register($x1),
            in("x2") Argument::to_register($x2),
            lateout("x0") primary,
            lateout("x1") secondary,
            options(nostack),
        );
        (primary, secondary)
    }};
}

#[cfg(all(target_os = "molecular", target_arch = "aarch64"))]
macro_rules! svc_noreturn {
    ($op:expr, $x0:expr) => {
        asm!(
            "svc #{op}",
            op = const $op as u16,
            in("x0") Argument::to_register($x0),
            options(noreturn, nostack),
        )
    };
}

#[cfg(not(all(target_os = "molecular", target_arch = "aarch64")))]
macro_rules! svc {
    ($op:expr $(, $arg:expr)*) => {{
        let _: u16 = $op as u16;
        $(let _: u64 = Argument::to_register($arg);)*
        let ret: i64 = -(SyscallError::BadSyscall as i64);
        ret
    }};
}

#[cfg(not(all(target_os = "molecular", target_arch = "aarch64")))]
macro_rules! svc_pair {
    ($op:expr $(, $arg:expr)*) => {{
        let _: u16 = $op as u16;
        $(let _: u64 = Argument::to_register($arg);)*
        (-(SyscallError::BadSyscall as i64), 0u64)
    }};
}

#[cfg(not(all(target_os = "molecular", target_arch = "aarch64")))]
macro_rules! svc_noreturn {
    ($op:expr, $x0:expr) => {{
        let _: u16 = $op as u16;
        let _: u64 = Argument::to_register($x0);
        loop {
            core::hint::spin_loop();
        }
    }};
}

use syscall::{Handle, IrqTrigger, SyscallError, SyscallOp, Timeout, WaitItem, WakeCount};

/// Ждёт сигналы `signals` на capability target `handle`. [`Timeout::POLL`] -
/// non-blocking poll. Возврат: observed-маска (>=0) либо `-(SyscallError)`.
pub fn signal_wait_one(handle: Handle, signals: u32, timeout: Timeout) -> i64 {
    // SAFETY: svc-immediate содержит номер операции (ESR.ISS). Аргументы лежат в x0..x2: handle,
    // маска сигналов, timeout_ns. Память не передаётся ядру. x0 на выходе: observed-маска (>=0)
    // либо -(SyscallError).
    unsafe { svc!(SyscallOp::SignalWaitOne, handle, signals, timeout.raw()) }
}

/// Меняет биты сигналов capability target `handle`: `set`/`clear` - нижние 32 бита,
/// `count` - политика пробуждения ([`WakeCount`]). Возврат: 0 либо
/// `-(SyscallError)`.
pub fn signal_set(handle: Handle, set: u32, clear: u32, count: WakeCount) -> i64 {
    // SAFETY: svc-immediate содержит номер операции (ESR.ISS). Аргументы лежат в x0..x3: handle,
    // set, clear, count. Память не передаётся ядру.
    unsafe { svc!(SyscallOp::SignalSet, handle, set, clear, count.to_raw()) }
}

/// Ждёт сигналы на нескольких capability target: `items` - записи `[handle, mask]`,
/// [`Timeout::POLL`] - non-blocking poll. Возврат: `(observed, index)` -
/// observed-маска сработавшего capability target (>=0 либо `-(SyscallError)`) и его индекс
/// в `items`.
pub fn signal_wait_many(items: &[WaitItem], timeout: Timeout) -> (i64, u64) {
    // SAFETY: svc-immediate содержит номер операции. Аргументы в x0..x2: items_va, count,
    // timeout_ns. Ядро читает массив записей по x0 (wire-layout WaitItem) до возврата из svc. На
    // выходе x0 - observed-маска, x1 - индекс.
    let (observed, index) = unsafe {
        svc_pair!(
            SyscallOp::SignalWaitMany,
            items.as_ptr() as u64,
            items.len() as u64,
            timeout.raw()
        )
    };
    (observed, index)
}

/// Создаёт пустой `Signal` в текущей handle-таблице. Возврат: handle
/// либо `-(SyscallError)`.
pub fn signal_create() -> Result<Handle, SyscallError> {
    // SAFETY: svc-immediate содержит номер операции. Нет аргументов. x0 на выходе - handle либо
    // -(SyscallError).
    let ret = unsafe { svc!(SyscallOp::SignalCreate) };
    Handle::from_syscall_return(ret)
}

/// Создаёт `Port` в текущей таблице.
/// Возврат: один port-handle либо `-(SyscallError)`.
pub fn port_create() -> Result<Handle, SyscallError> {
    // SAFETY: svc-immediate содержит номер операции. Нет аргументов. x0 на выходе - handle либо
    // -(SyscallError).
    let ret = unsafe { svc!(SyscallOp::PortCreate) };
    Handle::from_syscall_return(ret)
}

/// `send` на port `handle`: блокирующая отправка сообщения из
/// IPC-буфера текущего потока. `timeout`: [`Timeout::INFINITE`] - бессрочно,
/// [`Timeout::POLL`] - poll, иначе дедлайн. Требует `Rights::WRITE`.
/// Возврат: 0,
/// [`SYSCALL_RETURN_TIMEOUT`](syscall::SYSCALL_RETURN_TIMEOUT) при
/// истечении тайм-аута, либо `-(SyscallError)`.
pub fn port_send(handle: Handle, timeout: Timeout) -> i64 {
    // SAFETY: svc-immediate содержит номер операции, x0 - handle, x1 - timeout_ns. Сообщение лежит
    // в per-thread IPC-буфере (ядро само читает его). Память не передаётся по указателю.
    unsafe { svc!(SyscallOp::PortSend, handle, timeout.raw()) }
}

/// `recv` на port `handle`: блокирующий приём в IPC-буфер текущего
/// потока. `timeout` - как у [`port_send`]. Требует `Rights::READ`.
/// Возврат: reply handle id (если встречный был `call`, иначе 0),
/// [`SYSCALL_RETURN_TIMEOUT`](syscall::SYSCALL_RETURN_TIMEOUT) при
/// истечении тайм-аута, либо `-(SyscallError)`.
pub fn port_recv(handle: Handle, timeout: Timeout) -> i64 {
    // SAFETY: svc-immediate содержит номер операции, x0 - handle, x1 - timeout_ns. Ядро пишет
    // принятое сообщение в per-thread IPC-буфер текущего потока.
    unsafe { svc!(SyscallOp::PortRecv, handle, timeout.raw()) }
}

/// `call` на port `handle`: блокирующий запрос-ответ. Сообщение из
/// IPC-буфера текущего потока. Ответ оказывается там же. `timeout`
/// ограничивает всю операцию (см. [`port_send`]). Требует `Rights::WRITE`.
/// Возврат: 0,
/// [`SYSCALL_RETURN_TIMEOUT`](syscall::SYSCALL_RETURN_TIMEOUT) при
/// истечении тайм-аута, либо `-(SyscallError)`.
pub fn port_call(handle: Handle, timeout: Timeout) -> i64 {
    // SAFETY: svc-immediate содержит номер операции, x0 - handle, x1 - timeout_ns. Запрос и ответ
    // передаются через per-thread IPC-буфер текущего потока.
    unsafe { svc!(SyscallOp::PortCall, handle, timeout.raw()) }
}

/// `reply` на одноразовый `reply_handle` доставляет ответ из IPC-буфера сервера вызывателю. Требует
/// `Rights::WRITE`. Возврат: 0 либо `-(SyscallError)`.
pub fn port_reply(reply_handle: Handle) -> i64 {
    // SAFETY: svc-immediate содержит номер операции, x0 - reply handle. Ответ лежит в per-thread
    // IPC-буфере сервера.
    unsafe { svc!(SyscallOp::PortReply, reply_handle) }
}

/// Изымает `handle` из текущей таблицы и закрывает его. Возврат: 0 либо
/// `-(SyscallError)`.
pub fn handle_close(handle: Handle) -> i64 {
    // SAFETY: svc-immediate содержит номер операции, x0 - handle. Память не передаётся ядру.
    unsafe { svc!(SyscallOp::HandleClose, handle) }
}

/// Дублирует `handle` с правами `new_rights` (подмножество исходных,
/// неизвестные биты отбрасываются) и значком `badge` (set-once: `0` - без
/// значка / наследовать). Возврат: новый handle либо `-(SyscallError)`.
///
/// Семантика `badge`: заклеймить можно только незаклеймённый источник. Источник, который уже
/// заклеймён, наследует свой значок при `badge == 0`, а попытка переклеймить его (`badge != 0`)
/// даёт `BadHandle`.
pub fn handle_duplicate(
    handle: Handle,
    new_rights: u32,
    badge: u64,
) -> Result<Handle, SyscallError> {
    // SAFETY: svc-immediate содержит номер операции. Аргументы в x0..x2: handle, new_rights (нижние
    // 32 бита), badge (полные 64 бита). Память не передаётся ядру.
    let ret = unsafe { svc!(SyscallOp::HandleDuplicate, handle, new_rights, badge) };
    Handle::from_syscall_return(ret)
}

/// Создаёт пустой user-процесс с именем `name` (UTF-8). Возврат: handle
/// на `ProcessObject` либо `-(SyscallError)`.
pub fn process_create(name: &[u8]) -> Result<Handle, SyscallError> {
    // SAFETY: svc-immediate содержит номер операции, аргументы в x0..x1: name_va, name_len. Ядро
    // читает имя по x0 до возврата из svc.
    let ret = unsafe {
        svc!(
            SyscallOp::ProcessCreate,
            name.as_ptr() as u64,
            name.len() as u64
        )
    };
    Handle::from_syscall_return(ret)
}

/// Возвращает handle на собственный `ProcessObject` либо `-(SyscallError)`.
pub fn process_self() -> Result<Handle, SyscallError> {
    // SAFETY: svc-immediate содержит номер операции. Нет аргументов. x0 на выходе - handle либо
    // -(SyscallError).
    let ret = unsafe { svc!(SyscallOp::ProcessSelf) };
    Handle::from_syscall_return(ret)
}

/// Возвращает свежий handle на метеринг-`Resource` текущего процесса
/// (дефолтные права, включая `WRITE`). Возврат: resource-handle либо
/// `-(SyscallError)` (`WrongType`, если процесс стартовал без метеринг-ресурса).
pub fn process_resource_self() -> Result<Handle, SyscallError> {
    // SAFETY: svc-immediate содержит номер операции. Нет аргументов. x0 на выходе - handle либо
    // -(SyscallError).
    let ret = unsafe { svc!(SyscallOp::ProcessResourceSelf) };
    Handle::from_syscall_return(ret)
}

/// Загружает образ в процесс `handle` из сериализованного
/// `UserImageDescAbi` в `desc`. Требует `Rights::WRITE` на `handle`.
/// Возврат: 0 либо `-(SyscallError)`.
pub fn process_load_image(handle: Handle, desc: &[u8]) -> i64 {
    // SAFETY: svc-immediate содержит номер операции. Аргументы в x0..x2: process_handle, desc_va,
    // desc_len. Ядро копирует дескриптор и массив сегментов из user-памяти до возврата из svc.
    unsafe {
        svc!(
            SyscallOp::ProcessLoadImage,
            handle,
            desc.as_ptr() as u64,
            desc.len() as u64
        )
    }
}

/// Финальный exit-код процесса `handle`. Требует `Rights::READ`.
/// Возврат: exit code (нижние 32 бита, zero-extended) либо `-(SyscallError)`.
pub fn process_exit_code(handle: Handle) -> i64 {
    // SAFETY: svc-immediate содержит номер операции, x0 - handle. Память не передаётся ядру.
    unsafe { svc!(SyscallOp::ProcessExitCode, handle) }
}

/// Завершает процесс `handle` с кодом `exit_code`. Помечает завершёнными все его потоки, после
/// декремента до нуля - и сам процесс. Требует `Rights::WRITE`. Возврат: 0 либо `-(SyscallError)`.
pub fn process_terminate(handle: Handle, exit_code: u64) -> i64 {
    // SAFETY: svc-immediate содержит номер операции, аргументы в x0..x1: handle, exit_code. Память
    // не передаётся ядру.
    unsafe { svc!(SyscallOp::ProcessTerminate, handle, exit_code) }
}

/// Стартует первый поток процесса `handle`: `bootstrap_handle` - HandleId
/// стартового хэндла-канала. Требует `Rights::WRITE` на `handle` и `Rights::TRANSFER`
/// на `bootstrap_handle`. Процесс наследует метеринг-ресурс вызывающего.
/// Возврат: handle на `ThreadObject` либо `SyscallError`.
pub fn process_start(
    handle: Handle,
    entry_pc: u64,
    user_sp: u64,
    bootstrap_handle: u64,
    priority: u64,
    resource_handle: u64,
) -> Result<Handle, SyscallError> {
    // SAFETY: svc-immediate содержит номер операции, аргументы в x0..x5. Ядро переносит
    // bootstrap_handle потомку до возврата.
    let ret = unsafe {
        svc!(
            SyscallOp::ProcessStart,
            handle,
            entry_pc,
            user_sp,
            bootstrap_handle,
            priority,
            resource_handle
        )
    };
    Handle::from_syscall_return(ret)
}

/// Выделяет `budget_pages` страниц бюджета `resource` в производный
/// `Resource` с тем же физдиапазоном и потолком доступа. Возврат:
/// resource-handle либо `-(SyscallError)` (`ResourceExhausted` при нехватке
/// бюджета).
pub fn resource_split(resource: Handle, budget_pages: u64) -> Result<Handle, SyscallError> {
    // SAFETY: svc-immediate содержит номер операции, аргументы в x0..x1. x0 на выходе - handle либо
    // -(SyscallError).
    let ret = unsafe { svc!(SyscallOp::ResourceSplit, resource, budget_pages) };
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
) -> Result<Handle, SyscallError> {
    // SAFETY: svc-immediate содержит номер операции. Аргументы в x0..x4: process_handle, entry_pc,
    // user_sp, arg, priority. Память не передаётся ядру.
    let ret = unsafe {
        svc!(
            SyscallOp::ThreadCreate,
            process,
            entry_pc,
            user_sp,
            arg,
            priority
        )
    };
    Handle::from_syscall_return(ret)
}

/// Возвращает handle на собственный `ThreadObject` либо `-(SyscallError)`.
pub fn thread_self() -> Result<Handle, SyscallError> {
    // SAFETY: svc-immediate содержит номер операции. Нет аргументов. x0 на выходе - handle либо
    // -(SyscallError).
    let ret = unsafe { svc!(SyscallOp::ThreadSelf) };
    Handle::from_syscall_return(ret)
}

/// Финальный exit-код потока `handle`. Требует `Rights::READ`.
/// Возврат: exit code (нижние 32 бита, zero-extended) либо `-(SyscallError)`.
pub fn thread_exit_code(handle: Handle) -> i64 {
    // SAFETY: svc-immediate содержит номер операции, x0 - handle. Память не передаётся ядру.
    unsafe { svc!(SyscallOp::ThreadExitCode, handle) }
}

/// Завершает поток `handle` с кодом `exit_code` (нижние 32 бита). Требует `Rights::WRITE`.
/// Отвергает терминирование собственного потока через handle - для self-exit есть `thread_exit`.
/// Возврат: 0 либо `-(SyscallError)`.
pub fn thread_terminate(handle: Handle, exit_code: u64) -> i64 {
    // SAFETY: svc-immediate содержит номер операции, аргументы в x0..x1: handle, exit_code. Память
    // не передаётся ядру.
    unsafe { svc!(SyscallOp::ThreadTerminate, handle, exit_code) }
}

/// Создаёт Memory-регион с Virtual backing: `resource` - handle на
/// метеринг-`Resource` (требует `WRITE`), `size_bytes`, `access_mask`
/// (биты R/W/X). Возврат: region handle либо `-(SyscallError)`.
pub fn memory_create_virtual(
    resource: Handle,
    size_bytes: u64,
    access_mask: u64,
) -> Result<Handle, SyscallError> {
    // SAFETY: svc-immediate содержит номер операции, аргументы в x0..x2. Память не передаётся ядру.
    let ret = unsafe {
        svc!(
            SyscallOp::MemoryCreateVirtual,
            resource,
            size_bytes,
            access_mask
        )
    };
    Handle::from_syscall_return(ret)
}

/// Деривация региона поверх того же PA с профилем `profile_raw`: `region` -
/// handle на исходный регион (требует `DUPLICATE`). Возврат: region handle
/// либо `-(SyscallError)`.
pub fn memory_with_profile(region: Handle, profile_raw: u64) -> Result<Handle, SyscallError> {
    // SAFETY: svc-immediate содержит номер операции, аргументы в x0..x1: region_handle,
    // profile_raw. Память не передаётся ядру.
    let ret = unsafe { svc!(SyscallOp::MemoryWithProfile, region, profile_raw) };
    Handle::from_syscall_return(ret)
}

/// Деривация узкого под-региона: `region` - handle на исходный регион
/// (требует `DUPLICATE`), `offset` - page-aligned смещение, `size_bytes`,
/// `access_mask` (биты R/W/X, не шире гранта). Возврат: region handle либо
/// `-(SyscallError)`.
pub fn memory_slice(
    region: Handle,
    offset: u64,
    size_bytes: u64,
    access_mask: u64,
) -> Result<Handle, SyscallError> {
    // SAFETY: svc-immediate содержит номер операции, аргументы в x0..x3: region_handle, offset,
    // size_bytes, access_mask. Память не передаётся ядру.
    let ret = unsafe {
        svc!(
            SyscallOp::MemorySlice,
            region,
            offset,
            size_bytes,
            access_mask
        )
    };
    Handle::from_syscall_return(ret)
}

/// Маппит регион `handle` в текущий user-AS: `flags_raw` - `UserMemFlags`.
/// Возврат: базовый VA либо `-(SyscallError)`.
pub fn memory_map(handle: Handle, size_bytes: u64, flags_raw: u64) -> i64 {
    // SAFETY: svc-immediate содержит номер операции, аргументы в x0..x2. Память не передаётся ядру.
    unsafe { svc!(SyscallOp::MemoryMap, handle, size_bytes, flags_raw) }
}

/// Меняет флаги существующего маппинга по `va`: `flags_raw` -
/// `UserMemFlags`. Возврат: 0 либо `-(SyscallError)`.
pub fn memory_remap(va: u64, size_bytes: u64, flags_raw: u64) -> i64 {
    // SAFETY: svc-immediate содержит номер операции, аргументы в x0..x2. Память не передаётся ядру.
    unsafe { svc!(SyscallOp::MemoryRemap, va, size_bytes, flags_raw) }
}

/// Выделяет анонимный регион и маппит его в свободный VA: `resource` - handle
/// на метеринг-`Resource` (требует `WRITE`), `size_bytes`, `flags_raw` -
/// `UserMemFlags`. Возврат: базовый VA либо `-(SyscallError)`.
pub fn memory_allocate(resource: Handle, size_bytes: u64, flags_raw: u64) -> i64 {
    // SAFETY: svc-immediate содержит номер операции, аргументы в x0..x2. Память не передаётся ядру.
    unsafe { svc!(SyscallOp::MemoryAllocate, resource, size_bytes, flags_raw) }
}

/// Снимает маппинг по `va` (page-aligned) длиной `size_bytes` и возвращает
/// регион в free-list. Возврат: 0 либо `-(SyscallError)`.
pub fn memory_free(va: u64, size_bytes: u64) -> i64 {
    // SAFETY: svc-immediate содержит номер операции, аргументы в x0..x1: va, size_bytes. Память не
    // передаётся ядру.
    unsafe { svc!(SyscallOp::MemoryFree, va, size_bytes) }
}

/// Инспектирует Memory-регион `handle`. Возврат: `(size_bytes,
/// base_pa | (kind_tag << 3) | access_bits)` — первый элемент знаковый
/// (`-(SyscallError)` при ошибке), второй валиден только при успехе.
/// base_pa page-aligned, младшие 12 бит содержат kind/access. Для `Virtual`
/// base_pa = 0.
pub fn memory_region_inspect(handle: Handle) -> (i64, u64) {
    // SAFETY: svc-immediate содержит номер операции, x0 - handle. Ядро пишет size_bytes в x0,
    // base|kind|access - в x1.
    let (primary, secondary) = unsafe { svc_pair!(SyscallOp::MemoryRegionInspect, handle) };
    (primary, secondary)
}

/// Возвращает user-VA per-thread IPC-буфера текущего потока. Возврат:
/// VA (>0) либо `-(SyscallError)`, если у потока нет буфера.
pub fn ipc_buffer_addr() -> i64 {
    // SAFETY: svc-immediate содержит номер операции. Нет аргументов. x0 на выходе - user-VA буфера
    // (>0) либо -(SyscallError). Память не передаётся ядру.
    unsafe { svc!(SyscallOp::ThreadIpcBufferAddr) }
}

/// Минтит `IrqLine` по полномочию `control` для линии `irq` с триггером
/// `trigger`. Требует `Rights::WRITE` на `control` и попадания `irq` в его
/// диапазон. Возврат: irq-line handle либо `-(SyscallError)`. Срабатывание
/// ожидается через `signal_wait_one`/`signal_wait_many` прямо по возвращённому
/// handle (бит `SIGNALED`).
pub fn irq_mint(control: Handle, irq: u16, trigger: IrqTrigger) -> Result<Handle, SyscallError> {
    // SAFETY: svc-immediate содержит номер операции, x0 - control handle, x1 - номер линии, x2 -
    // код триггера. Память не передаётся ядру.
    let ret = unsafe { svc!(SyscallOp::IrqMint, control, irq, trigger.code()) };
    Handle::from_syscall_return(ret)
}

/// Подтверждает прерывание на `line`. Снимает latch `SIGNALED` и размаскирует линию. Требует
/// `Rights::WRITE`. Возврат: 0 либо `-(SyscallError)`.
pub fn irq_ack(line: Handle) -> i64 {
    // SAFETY: svc-immediate содержит номер операции, x0 - line handle. Память не передаётся ядру.
    unsafe { svc!(SyscallOp::IrqAck, line) }
}

/// Завершает текущий поток с кодом `code`. Не возвращается.
pub fn thread_exit(code: u64) -> ! {
    // SAFETY: ThreadExit не возвращается, поэтому asm помечен noreturn и
    // удовлетворяет `-> !`. x0 содержит exit code.
    unsafe { svc_noreturn!(SyscallOp::ThreadExit, code) }
}

/// Завершает текущий процесс со всеми его потоками с кодом `code`.
/// Не возвращается.
pub fn process_exit(code: u64) -> ! {
    // SAFETY: ProcessExit не возвращается, поэтому asm помечен noreturn и
    // удовлетворяет `-> !`. x0 содержит exit code.
    unsafe { svc_noreturn!(SyscallOp::ProcessExit, code) }
}

/// Уступает остаток кванта текущего потока.
pub fn thread_yield() {
    // SAFETY: svc-immediate содержит номер операции. Нет аргументов. Память не передаётся ядру,
    // возврат не используется.
    unsafe { svc!(SyscallOp::ThreadYield) };
}
