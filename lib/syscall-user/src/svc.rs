//! Inline-asm реализация svc-обёрток.
//!
//! Макросы ставят ловушку. Номер операции идёт immediate-полем `svc`, аргументы - в x0 и
//! дальше по порядку, возврат - в x0. Обёртка остаётся типизированной подписью с обоснованием
//! своего `unsafe`.
//!
//! Ловушка существует только под RustOS на aarch64. На любом другом таргете возвращающие
//! макросы отдают код `BadSyscall`, а `svc_noreturn` крутит пустой цикл, и крейт собирается
//! для линтов и хостовых тестов зависимых крейтов. Блоки `unsafe` обёрток при этом остаются
//! без unsafe-операции, и предупреждение о них снято на весь модуль.

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

impl<K: ObjectKind> Argument for Handle<K> {
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

use syscall::{
    Handle, IrqTrigger, ObjectKind, SyscallError, SyscallOp, Timeout, WaitItem, WaitableKind,
    WakeCount, kind,
};

use crate::IssuedHandle;

/// Ждёт сигналы `signals` на capability target `handle`.
///
/// [`Timeout::POLL`] делает вызов неблокирующим опросом. Требует `Rights::READ`, а нулевая
/// маска даёт `InvalidArgument`. Закрытие либо передача `handle` до сигнала снимает ожидание
/// кодом `Canceled`.
/// Возврат: observed-маска (>=0) либо `-(SyscallError)`.
pub fn signal_wait_one<K: WaitableKind>(handle: Handle<K>, signals: u32, timeout: Timeout) -> i64 {
    // SAFETY: svc-immediate содержит номер операции (ESR.ISS). Аргументы лежат в x0..x2: handle,
    // маска сигналов, timeout_ns. Память не передаётся ядру. x0 на выходе: observed-маска (>=0)
    // либо -(SyscallError).
    unsafe { svc!(SyscallOp::SignalWaitOne, handle, signals, timeout.raw()) }
}

/// Меняет биты сигналов capability target `handle`.
/// Аргументы:
///  - `set` и `clear` - нижние 32 бита каждый,
///  - `count` - политика пробуждения ([`WakeCount`]).
///
/// Биты меняются при любом `count`. Требует `Rights::WRITE` и принимает только `Signal`,
/// на другом объекте - `WrongType`.
/// Возврат: 0 либо `-(SyscallError)`.
pub fn signal_set(handle: Handle<kind::Signal>, set: u32, clear: u32, count: WakeCount) -> i64 {
    // SAFETY: svc-immediate содержит номер операции (ESR.ISS). Аргументы лежат в x0..x3: handle,
    // set, clear, count. Память не передаётся ядру.
    unsafe { svc!(SyscallOp::SignalSet, handle, set, clear, count.to_raw()) }
}

/// Ждёт сигналы сразу на нескольких capability target.
/// Аргументы:
///  - `items` - записи `[handle, mask]`,
///  - `timeout` - [`Timeout::POLL`] делает вызов неблокирующим опросом.
///
/// Длина `items` вне `1..=`[`WAIT_MANY_MAX_COUNT`](syscall::WAIT_MANY_MAX_COUNT) даёт
/// `InvalidArgument`, нулевая маска записи - тоже. Требует `Rights::READ` на каждом handle,
/// причём повтор одного handle допустим. Ядро читает массив по указателю до возврата из ловушки.
/// Возврат: `(observed, index)` - observed-маска сработавшего capability target
/// (>=0 либо `-(SyscallError)`) и индекс сработавшей записи в `items`.
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

/// Создаёт `Signal` и ставит handle на него в текущую таблицу со стартовыми правами
/// `READ | WRITE | DUPLICATE | TRANSFER`. Возврат: handle либо `-(SyscallError)`.
pub fn signal_create() -> Result<IssuedHandle<kind::Signal>, SyscallError> {
    // SAFETY: svc-immediate содержит номер операции. Нет аргументов. x0 на выходе - handle либо
    // -(SyscallError).
    let ret = unsafe { svc!(SyscallOp::SignalCreate) };
    IssuedHandle::from_syscall_return(ret)
}

/// Создаёт `Port` и ставит один handle на него в текущую таблицу со стартовыми правами
/// `READ | WRITE | DUPLICATE | TRANSFER`. Обе роли порта берутся с этого handle
/// дупликацией. Возврат: port-handle либо `-(SyscallError)`.
pub fn port_create() -> Result<IssuedHandle<kind::Port>, SyscallError> {
    // SAFETY: svc-immediate содержит номер операции. Нет аргументов. x0 на выходе - handle либо
    // -(SyscallError).
    let ret = unsafe { svc!(SyscallOp::PortCreate) };
    IssuedHandle::from_syscall_return(ret)
}

/// `send` на port `handle` - блокирующая отправка сообщения из IPC-буфера текущего потока.
/// Значения `timeout`:
///  - [`Timeout::INFINITE`] - ждать бессрочно,
///  - [`Timeout::POLL`] - опрос без блокировки,
///  - прочее - относительный дедлайн.
///
/// Требует `Rights::WRITE`. Порт без получателей отвечает `PeerClosed`, не блокируясь.
/// Перечисленные в `IpcBuffer.caps` handle требуют `Rights::TRANSFER` и уходят из таблицы
/// отправителя на доставке, а отказ оставляет их на месте.
/// Возврат: 0,
/// [`SYSCALL_RETURN_TIMEOUT`](syscall::SYSCALL_RETURN_TIMEOUT) при
/// истечении тайм-аута, либо `-(SyscallError)`.
pub fn port_send(handle: Handle<kind::Port>, timeout: Timeout) -> i64 {
    // SAFETY: svc-immediate содержит номер операции, x0 - handle, x1 - timeout_ns. Сообщение лежит
    // в per-thread IPC-буфере (ядро само читает его). Память не передаётся по указателю.
    unsafe { svc!(SyscallOp::PortSend, handle, timeout.raw()) }
}

/// `recv` на port `handle` - блокирующий приём в IPC-буфер текущего потока.
///
/// `timeout` толкуется как у [`port_send`]. Требует `Rights::READ`. Значок отправителя ядро
/// кладёт в `IpcBuffer.badge`, а принятые handle вставляет в таблицу получателя.
/// Возврат: владелец reply handle со стартовыми правами `WRITE | TRANSFER`, если встречный был
/// `call`, иначе `None`. Истечение тайм-аута даёт `SyscallError::Timeout`.
pub fn port_recv(
    handle: Handle<kind::Port>,
    timeout: Timeout,
) -> Result<Option<IssuedHandle<kind::Reply>>, SyscallError> {
    // SAFETY: svc-immediate содержит номер операции, x0 - handle, x1 - timeout_ns. Ядро пишет
    // принятое сообщение в per-thread IPC-буфер текущего потока.
    let ret = unsafe { svc!(SyscallOp::PortRecv, handle, timeout.raw()) };
    if ret == 0 {
        return Ok(None);
    }
    IssuedHandle::from_syscall_return(ret).map(Some)
}

/// `call` на port `handle` - блокирующий запрос-ответ. Запрос берётся из IPC-буфера текущего
/// потока, ответ оказывается там же.
///
/// `timeout` ограничивает всю операцию целиком (см. [`port_send`]). Требует `Rights::WRITE`.
/// Порт без получателей отвечает `PeerClosed`, не блокируясь. Уход по тайм-ауту оставляет
/// IPC-буфер вызывателя нетронутым, а серверный [`port_reply`] на таком кадре получает
/// `PeerClosed`.
/// Возврат: 0,
/// [`SYSCALL_RETURN_TIMEOUT`](syscall::SYSCALL_RETURN_TIMEOUT) при
/// истечении тайм-аута, либо `-(SyscallError)`.
pub fn port_call(handle: Handle<kind::Port>, timeout: Timeout) -> i64 {
    // SAFETY: svc-immediate содержит номер операции, x0 - handle, x1 - timeout_ns. Запрос и ответ
    // передаются через per-thread IPC-буфер текущего потока.
    unsafe { svc!(SyscallOp::PortCall, handle, timeout.raw()) }
}

/// `reply` на одноразовый `reply_handle` доставляет вызывателю ответ из IPC-буфера сервера.
///
/// Требует `Rights::WRITE`. Handle изымается из таблицы сервера в любом случае, отказ доставки
/// включая, так что повторный вызов на нём даёт `BadHandle`.
/// Возврат: 0 либо `-(SyscallError)`.
pub fn port_reply(reply_handle: IssuedHandle<kind::Reply>) -> i64 {
    let reply_handle = reply_handle.leak();
    // SAFETY: svc-immediate содержит номер операции, x0 - reply handle. Ответ лежит в per-thread
    // IPC-буфере сервера.
    unsafe { svc!(SyscallOp::PortReply, reply_handle) }
}

/// Изымает `handle` из текущей таблицы и закрывает его.
///
/// Прав не требует. Закрытие отзывает капы, выведенные из этого handle, а сам объект
/// освобождается с уходом последней ссылки. Неизвестный id - `BadHandle`.
/// Возврат: 0 либо `-(SyscallError)`.
pub fn handle_close<K: ObjectKind>(handle: IssuedHandle<K>) -> i64 {
    close_entry(handle.leak())
}

pub(crate) fn close_entry<K: ObjectKind>(handle: Handle<K>) -> i64 {
    // SAFETY: svc-immediate содержит номер операции, x0 - handle. Память не передаётся ядру.
    unsafe { svc!(SyscallOp::HandleClose, handle) }
}

/// Дублирует `handle` с урезанными правами и, если нужно, со значком.
/// Аргументы:
///  - `new_rights` - подмножество прав источника, неизвестные биты отбрасываются,
///  - `badge` - значок set-once. Ноль означает "без значка либо наследовать".
///
/// Требует `Rights::DUPLICATE`, а `new_rights` вне прав источника дают `AccessDenied`. Новый
/// handle становится потомком исходного по цепочке деривации, и закрытие источника его
/// отзывает.
/// Возврат: новый handle либо `-(SyscallError)`.
///
/// Семантика `badge`: заклеймить можно только незаклеймённый источник. Источник, который уже
/// заклеймён, наследует свой значок при `badge == 0`, а попытка переклеймить его (`badge != 0`)
/// даёт `BadHandle`.
pub fn handle_duplicate<K: ObjectKind>(
    handle: Handle<K>,
    new_rights: u32,
    badge: u64,
) -> Result<IssuedHandle<K>, SyscallError> {
    // SAFETY: svc-immediate содержит номер операции. Аргументы в x0..x2: handle, new_rights (нижние
    // 32 бита), badge (полные 64 бита). Память не передаётся ядру.
    let ret = unsafe { svc!(SyscallOp::HandleDuplicate, handle, new_rights, badge) };
    IssuedHandle::from_syscall_return(ret)
}

/// Создаёт пустой user-процесс с именем `name` - UTF-8 длиной в
/// `1..=`[`MAX_PROCESS_NAME_LEN`](syscall::MAX_PROCESS_NAME_LEN).
///
/// У свежего процесса ещё нет образа, так что за `process_create` идут
/// [`process_load_image`] и [`process_start`].
/// Возврат: handle на `ProcessObject` либо `-(SyscallError)`.
pub fn process_create(name: &[u8]) -> Result<IssuedHandle<kind::Process>, SyscallError> {
    // SAFETY: svc-immediate содержит номер операции, аргументы в x0..x1: name_va, name_len. Ядро
    // читает имя по x0 до возврата из svc.
    let ret = unsafe {
        svc!(
            SyscallOp::ProcessCreate,
            name.as_ptr() as u64,
            name.len() as u64
        )
    };
    IssuedHandle::from_syscall_return(ret)
}

/// Возвращает handle на собственный `ProcessObject` либо `-(SyscallError)`.
pub fn process_self() -> Result<IssuedHandle<kind::Process>, SyscallError> {
    // SAFETY: svc-immediate содержит номер операции. Нет аргументов. x0 на выходе - handle либо
    // -(SyscallError).
    let ret = unsafe { svc!(SyscallOp::ProcessSelf) };
    IssuedHandle::from_syscall_return(ret)
}

/// Возвращает свежий handle на метеринг-`Resource` текущего процесса.
///
/// Права капы - те, с которыми ресурс достался процессу на старте. Шире прав того handle,
/// которым ресурс передали, процессу не достаётся ничего.
/// Возврат: resource-handle либо `-(SyscallError)`. Процесс, стартовавший без
/// метеринг-ресурса, получает `WrongType`.
pub fn process_resource_self() -> Result<IssuedHandle<kind::Resource>, SyscallError> {
    // SAFETY: svc-immediate содержит номер операции. Нет аргументов. x0 на выходе - handle либо
    // -(SyscallError).
    let ret = unsafe { svc!(SyscallOp::ProcessResourceSelf) };
    IssuedHandle::from_syscall_return(ret)
}

/// Загружает образ в процесс `handle` из сериализованного `UserImageDescAbi` в `desc`.
///
/// Длина `desc` обязана равняться [`USER_IMAGE_DESC_SIZE`](syscall::USER_IMAGE_DESC_SIZE),
/// иначе вызов даёт `InvalidArgument`. Требует `Rights::WRITE` на `handle`, а на каждом
/// region-handle сегмента - `Rights::TRANSFER` и права по флагам самого сегмента.
/// Region-handle сегментов остаются у вызывающего.
/// Возврат: 0 либо `-(SyscallError)`.
pub fn process_load_image(handle: Handle<kind::Process>, desc: &[u8]) -> i64 {
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

/// Финальный exit-код процесса `handle`.
///
/// Требует `Rights::READ`. Пока процесс не завершился, вызов отдаёт 0. Само завершение
/// отслеживают ожиданием бита `SIGNALED` на этом же handle.
/// Возврат: exit code (нижние 32 бита, zero-extended) либо `-(SyscallError)`.
pub fn process_exit_code(handle: Handle<kind::Process>) -> i64 {
    // SAFETY: svc-immediate содержит номер операции, x0 - handle. Память не передаётся ядру.
    unsafe { svc!(SyscallOp::ProcessExitCode, handle) }
}

/// Завершает процесс `handle` с кодом `exit_code` - его нижними 32 битами.
///
/// Помечает завершёнными все потоки процесса, а после декремента до нуля - и сам процесс.
/// Требует `Rights::WRITE`. Снять через handle собственный процесс нельзя, такой вызов
/// отвергается кодом `AccessDenied`, а для self-exit есть [`process_exit`].
/// Возврат: 0 либо `-(SyscallError)`.
pub fn process_terminate(handle: Handle<kind::Process>, exit_code: u64) -> i64 {
    // SAFETY: svc-immediate содержит номер операции, аргументы в x0..x1: handle, exit_code. Память
    // не передаётся ядру.
    unsafe { svc!(SyscallOp::ProcessTerminate, handle, exit_code) }
}

/// Стартует первый поток процесса `handle`.
/// Аргументы:
///  - `bootstrap` - владелец стартового хэндла-канала,
///  - `priority` - приоритет в битах `[0..8)`.
///
/// Требует `Rights::WRITE` на `handle` и `Rights::TRANSFER` на `bootstrap`. Стартовый хэндл
/// уходит из таблицы вызывающего в таблицу потомка без права `Rights::TRANSFER`, и X0 первого
/// потока получает его id уже в child-таблице. Отказ до старта потока оставляет запись у
/// вызывающего, и её закрывает владелец `bootstrap`. Отказ вставки handle потока завершает
/// запущенного потомка вместе со стартовым хэндлом.
///
/// Метеринг-ресурс потомку достаётся одним из двух путей. Нулевой `resource_handle` отдаёт
/// ресурс вызывающего с правами его же гранта. Ненулевой требует
/// `Rights::WRITE | Rights::DUPLICATE` и достаётся потомку с правами этого handle.
/// Возврат: handle на `ThreadObject` либо `SyscallError`.
pub fn process_start<K: ObjectKind>(
    handle: Handle<kind::Process>,
    entry_pc: u64,
    user_sp: u64,
    bootstrap: IssuedHandle<K>,
    priority: u64,
    resource: Option<Handle<kind::Resource>>,
) -> Result<IssuedHandle<kind::Thread>, SyscallError> {
    // Отсутствие метеринг-ресурса ядро принимает нулём в регистре.
    let resource_handle = resource.map_or(0, |handle| u64::from(handle.raw()));
    // SAFETY: svc-immediate содержит номер операции, аргументы в x0..x5. Ядро переносит
    // стартовый хэндл потомку до возврата.
    let ret = unsafe {
        svc!(
            SyscallOp::ProcessStart,
            handle,
            entry_pc,
            user_sp,
            bootstrap.as_raw(),
            priority,
            resource_handle
        )
    };
    let thread = IssuedHandle::from_syscall_return(ret)?;
    // Ядро перенесло стартовый хэндл в таблицу потомка, и в таблице вызывающего нет этой записи.
    let _ = bootstrap.leak();
    Ok(thread)
}

/// Выделяет `budget_pages` страниц бюджета `resource` в производный `Resource` с тем же
/// потолком доступа.
///
/// Требует `Rights::WRITE | Rights::DUPLICATE`, а нулевой `budget_pages` даёт
/// `InvalidArgument`. Страницы возвращаются исходному ресурсу с уходом производного, а
/// отзыв исходной капы отзывает и производную.
/// Возврат:
/// resource-handle либо `-(SyscallError)` (`ResourceExhausted` при нехватке бюджета и на
/// цепочке глубиной [`MAX_RESOURCE_DEPTH`](syscall::MAX_RESOURCE_DEPTH)).
pub fn resource_split(
    resource: Handle<kind::Resource>,
    budget_pages: u64,
) -> Result<IssuedHandle<kind::Resource>, SyscallError> {
    // SAFETY: svc-immediate содержит номер операции, аргументы в x0..x1. x0 на выходе - handle либо
    // -(SyscallError).
    let ret = unsafe { svc!(SyscallOp::ResourceSplit, resource, budget_pages) };
    IssuedHandle::from_syscall_return(ret)
}

/// Создаёт поток в процессе `process`.
/// Аргументы:
///  - `entry_pc` и `user_sp` - точка входа и вершина стека,
///  - `arg` - значение X0 первой инструкции,
///  - `priority` - приоритет в битах `[0..8)`, свыше `0xFF` - `InvalidArgument`.
///
/// Требует `Rights::WRITE` на `process`. Процесс без загруженного образа отвечает
/// `WrongType`. Ядро отображает IPC-буфер потока в адресное пространство `process`, и
/// заполненная таблица его отображений даёт `OutOfMappings`.
/// Возврат: handle на `ThreadObject` либо `-(SyscallError)`.
pub fn thread_create(
    process: Handle<kind::Process>,
    entry_pc: u64,
    user_sp: u64,
    arg: u64,
    priority: u64,
) -> Result<IssuedHandle<kind::Thread>, SyscallError> {
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
    IssuedHandle::from_syscall_return(ret)
}

/// Возвращает handle на собственный `ThreadObject` либо `-(SyscallError)`.
pub fn thread_self() -> Result<IssuedHandle<kind::Thread>, SyscallError> {
    // SAFETY: svc-immediate содержит номер операции. Нет аргументов. x0 на выходе - handle либо
    // -(SyscallError).
    let ret = unsafe { svc!(SyscallOp::ThreadSelf) };
    IssuedHandle::from_syscall_return(ret)
}

/// Финальный exit-код потока `handle`.
///
/// Требует `Rights::READ`. Пока поток не завершился, вызов отдаёт 0.
/// Возврат: exit code (нижние 32 бита, zero-extended) либо `-(SyscallError)`.
pub fn thread_exit_code(handle: Handle<kind::Thread>) -> i64 {
    // SAFETY: svc-immediate содержит номер операции, x0 - handle. Память не передаётся ядру.
    unsafe { svc!(SyscallOp::ThreadExitCode, handle) }
}

/// Завершает поток `handle` с кодом `exit_code` - его нижними 32 битами.
///
/// Требует `Rights::WRITE`. Снять через handle собственный поток нельзя, такой вызов
/// отвергается кодом `AccessDenied`, а для self-exit есть [`thread_exit`].
/// Возврат: 0 либо `-(SyscallError)`.
pub fn thread_terminate(handle: Handle<kind::Thread>, exit_code: u64) -> i64 {
    // SAFETY: svc-immediate содержит номер операции, аргументы в x0..x1: handle, exit_code. Память
    // не передаётся ядру.
    unsafe { svc!(SyscallOp::ThreadTerminate, handle, exit_code) }
}

/// Создаёт Memory-регион с Virtual backing.
/// Аргументы:
///  - `resource` - handle на метеринг-`Resource`, требует `WRITE`,
///  - `size_bytes` - ненулевой и кратный [`PAGE_SIZE`](syscall::PAGE_SIZE),
///  - `access_mask` - ненулевая маска битов R/W/X,
///  - `region_flags` - [`MEMORY_REGION_CONTIGUOUS`](syscall::MEMORY_REGION_CONTIGUOUS) либо
///    ноль.
///
/// С ресурса списывается `size_bytes / PAGE_SIZE` страниц. Обратно они возвращаются с
/// уходом последней ссылки на регион.
/// Возврат: region handle либо `-(SyscallError)`. Нехватка бюджета даёт
/// `ResourceExhausted`. Без свободного смежного участка для непрерывного региона вызов
/// возвращает `OutOfMemory`.
pub fn memory_create_virtual(
    resource: Handle<kind::Resource>,
    size_bytes: u64,
    access_mask: u64,
    region_flags: u64,
) -> Result<IssuedHandle<kind::MemoryRegion>, SyscallError> {
    // SAFETY: svc-immediate содержит номер операции, аргументы в x0..x3. Память не передаётся ядру.
    let ret = unsafe {
        svc!(
            SyscallOp::MemoryCreateVirtual,
            resource,
            size_bytes,
            access_mask,
            region_flags
        )
    };
    IssuedHandle::from_syscall_return(ret)
}

/// Выводит узкий под-регион исходного.
/// Аргументы:
///  - `region` - handle на исходный регион, требует `DUPLICATE`,
///  - `offset` - page-aligned смещение окна,
///  - `size_bytes` - длина окна,
///  - `access_mask` - биты R/W/X, не шире гранта,
///  - `region_flags` -
///    [`MEMORY_REGION_WRITE_COMBINING`](syscall::MEMORY_REGION_WRITE_COMBINING) либо ноль
///    для профиля исходного региона.
///
/// Грант - это права handle, суженные access-маской региона, а превышение даёт
/// `AccessDenied`. Окно `[offset, offset+size_bytes)` обязано укладываться в исходный
/// регион. Нарезка ничего не аллоцирует и бюджета не списывает, а под-регион становится
/// капой-потомком исходной.
/// Возврат: region handle либо `-(SyscallError)`.
pub fn memory_slice(
    region: Handle<kind::MemoryRegion>,
    offset: u64,
    size_bytes: u64,
    access_mask: u64,
    region_flags: u64,
) -> Result<IssuedHandle<kind::MemoryRegion>, SyscallError> {
    // SAFETY: svc-immediate содержит номер операции, аргументы в x0..x4: region_handle, offset,
    // size_bytes, access_mask, region_flags. Память не передаётся ядру.
    let ret = unsafe {
        svc!(
            SyscallOp::MemorySlice,
            region,
            offset,
            size_bytes,
            access_mask,
            region_flags
        )
    };
    IssuedHandle::from_syscall_return(ret)
}

/// Маппит регион `handle` в текущий user-AS на свободный VA. `flags_raw` - это
/// `UserMemFlags`, а `size_bytes` равен полному размеру региона.
///
/// Права на handle нужны по `flags_raw`:
///  - `READ` для RO,
///  - `READ | WRITE` для RW,
///  - `READ | EXECUTE` для RX.
///
/// Маппинг снимается отзывом `handle`. Капа, отозванная в гонке, оставляет отказ `Revoked`
/// вместо живого VA. Заполненная таблица отображений процесса даёт `OutOfMappings`.
/// Возврат: базовый VA либо `-(SyscallError)`.
pub fn memory_map(handle: Handle<kind::MemoryRegion>, size_bytes: u64, flags_raw: u64) -> i64 {
    // SAFETY: svc-immediate содержит номер операции, аргументы в x0..x2. Память не передаётся ядру.
    unsafe { svc!(SyscallOp::MemoryMap, handle, size_bytes, flags_raw) }
}

/// Меняет флаги существующего маппинга. `va` - page-aligned база маппинга, `flags_raw` -
/// новые `UserMemFlags`.
///
/// Регион остаётся прежним, и handle на этом шаге вызов не проверяет. Расширение доступа
/// ограничено грантом, снятым при [`memory_map`] либо [`memory_allocate`]. Превышение даёт
/// `InvalidArgument`, а неизвестный диапазон - `NotFound`.
/// Возврат: 0 либо `-(SyscallError)`.
pub fn memory_remap(va: u64, size_bytes: u64, flags_raw: u64) -> i64 {
    // SAFETY: svc-immediate содержит номер операции, аргументы в x0..x2. Память не передаётся ядру.
    unsafe { svc!(SyscallOp::MemoryRemap, va, size_bytes, flags_raw) }
}

/// Выделяет анонимный регион и сразу маппит его в свободный VA.
/// Аргументы:
///  - `resource` - handle на метеринг-`Resource`, требует `WRITE`,
///  - `size_bytes` - ненулевой и кратный [`PAGE_SIZE`](syscall::PAGE_SIZE),
///  - `flags_raw` - `UserMemFlags`.
///
/// Handle на регион наружу не выкладывается, и доступ целиком задаётся `flags_raw`. Бюджет
/// возвращается ресурсу на [`memory_free`] либо с разрушением адресного пространства.
/// Заполненная таблица отображений процесса даёт `OutOfMappings`, как у [`memory_map`].
/// Возврат: базовый VA либо `-(SyscallError)`.
pub fn memory_allocate(resource: Handle<kind::Resource>, size_bytes: u64, flags_raw: u64) -> i64 {
    // SAFETY: svc-immediate содержит номер операции, аргументы в x0..x2. Память не передаётся ядру.
    unsafe { svc!(SyscallOp::MemoryAllocate, resource, size_bytes, flags_raw) }
}

/// Снимает маппинг по page-aligned `va` длиной `size_bytes` и возвращает регион в
/// free-list.
///
/// Невыделенный диапазон отвечает `NotFound`. Уход последней ссылки на регион возвращает
/// его страницы аллокатору фреймов, а списанный бюджет - ресурсу.
/// Возврат: 0 либо `-(SyscallError)`.
pub fn memory_free(va: u64, size_bytes: u64) -> i64 {
    // SAFETY: svc-immediate содержит номер операции, аргументы в x0..x1: va, size_bytes. Память не
    // передаётся ядру.
    unsafe { svc!(SyscallOp::MemoryFree, va, size_bytes) }
}

/// Инспектирует Memory-регион `handle`. Требует `Rights::READ`.
///
/// Возврат: пара `(size_bytes, base_pa | (profile_raw << 5) | (kind_tag << 3) |
/// access_bits)`. Первый элемент знаковый и при ошибке равен `-(SyscallError)`, второй
/// валиден только при успехе.
///
/// `base_pa` page-aligned, так что его младшие 12 бит свободны под остальные поля:
/// `access_bits` в `[0..3)`, `kind_tag` в `[3..5)` и `profile_raw` в `[5..7)`. У
/// непрерывного `Virtual`-региона `base_pa` равен адресу первой страницы, у прочих
/// `Virtual`-регионов `base_pa` равен нулю.
pub fn memory_region_inspect(handle: Handle<kind::MemoryRegion>) -> (i64, u64) {
    // SAFETY: svc-immediate содержит номер операции, x0 - handle. Ядро пишет size_bytes в x0,
    // base|kind|access - в x1.
    let (primary, secondary) = unsafe { svc_pair!(SyscallOp::MemoryRegionInspect, handle) };
    (primary, secondary)
}

/// Возвращает user-VA per-thread IPC-буфера текущего потока. Буфер занимает страницу,
/// замапленную user-RW, и у каждого потока он свой. Возврат:
/// VA (>0) либо `-(SyscallError)`, у потока ядра - `WrongType`.
pub fn ipc_buffer_addr() -> i64 {
    // SAFETY: svc-immediate содержит номер операции. Нет аргументов. x0 на выходе - user-VA буфера
    // (>0) либо -(SyscallError). Память не передаётся ядру.
    unsafe { svc!(SyscallOp::ThreadIpcBufferAddr) }
}

/// Минтит `IrqLine` по полномочию `control` для линии `irq` с триггером `trigger`.
///
/// Требует `Rights::WRITE` на `control` и попадания `irq` в его диапазон, а номер вне
/// диапазона даёт `AccessDenied`. Линия минтится ровно один раз, и повторный минт занятой
/// линии отвечает `AlreadyBound`.
/// Возврат: irq-line handle либо `-(SyscallError)`. Срабатывание ожидают через
/// `signal_wait_one`/`signal_wait_many` прямо по возвращённому handle, по биту
/// `SIGNALED`.
pub fn irq_mint(
    control: Handle<kind::IrqControl>,
    irq: u16,
    trigger: IrqTrigger,
) -> Result<IssuedHandle<kind::IrqLine>, SyscallError> {
    // SAFETY: svc-immediate содержит номер операции, x0 - control handle, x1 - номер линии, x2 -
    // код триггера. Память не передаётся ядру.
    let ret = unsafe { svc!(SyscallOp::IrqMint, control, irq, trigger.code()) };
    IssuedHandle::from_syscall_return(ret)
}

/// Подтверждает прерывание на `line`, снимая latch `SIGNALED` и размаскируя линию.
///
/// Требует `Rights::WRITE`.
/// Возврат: 0 либо `-(SyscallError)`.
pub fn irq_ack(line: Handle<kind::IrqLine>) -> i64 {
    // SAFETY: svc-immediate содержит номер операции, x0 - line handle. Память не передаётся ядру.
    unsafe { svc!(SyscallOp::IrqAck, line) }
}

/// Завершает текущий поток с кодом `code` (нижние 32 бита). На последнем потоке процесса
/// завершается и процесс. Не возвращается. Вне RustOS на aarch64 крутит пустой цикл.
pub fn thread_exit(code: u64) -> ! {
    // SAFETY: ThreadExit не возвращается, поэтому asm помечен noreturn и
    // удовлетворяет `-> !`. x0 содержит exit code.
    unsafe { svc_noreturn!(SyscallOp::ThreadExit, code) }
}

/// Завершает текущий процесс со всеми его потоками с кодом `code` (нижние 32 бита).
/// Не возвращается. Вне RustOS на aarch64 крутит пустой цикл.
pub fn process_exit(code: u64) -> ! {
    // SAFETY: ProcessExit не возвращается, поэтому asm помечен noreturn и
    // удовлетворяет `-> !`. x0 содержит exit code.
    unsafe { svc_noreturn!(SyscallOp::ProcessExit, code) }
}

/// Уступает остаток кванта текущего потока. Поток встаёт в конец своей ready-очереди и
/// остаётся runnable.
pub fn thread_yield() {
    // SAFETY: svc-immediate содержит номер операции. Нет аргументов. Память не передаётся ядру,
    // возврат не используется.
    unsafe { svc!(SyscallOp::ThreadYield) };
}
