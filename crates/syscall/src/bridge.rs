//! Архитектурно-независимая точка входа syscall-слоя.
//!
//! `SyscallFrame` - абстракция над регистровым контекстом, в которой
//! хранятся номер операции, аргументы и возвращаемое значение. Каждая
//! архитектура реализует этот trait для собственного trap-фрейма.
//!
//! ## Источник вызова
//!
//! [`Origin`] различает syscall'ы, инициированные из user-контекста и
//! из самого ядра. Kernel-side IPC ходит напрямую через `kobject`, а
//! trap из [`Origin::Kernel`] отвергается с
//! [`SyscallError::KernelOriginated`] до диспатча - syscall-trap
//! зарезервирован исключительно для user->kernel-перехода.

use core::num::NonZeroU32;

use kobject::{self, HandleId, Rights};

use super::{
    error::{SyscallError, encode_return},
    numbers::SyscallOp,
    runtime::runtime as syscall_runtime,
    user_io::{copy_in, validate_user_ptr},
};

/// Источник syscall-вызова.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Вызов пришёл из user-контекста (отдельный процесс).
    User,
    /// Вызов пришёл из kernel-контекста (kernel-thread).
    Kernel,
}

/// Доступ к регистровому контексту syscall-вызова. Реализуется
/// платформенным слоем для своего trap-фрейма.
pub trait SyscallFrame {
    /// 16-битный номер операции, выбранный платформой при входе в trap.
    fn op_raw(&self) -> u16;

    /// Аргумент `i` (0..=5). Для `i >= 6` поведение не определено.
    fn arg(&self, i: usize) -> u64;

    /// Записать возвращаемое значение в регистр-возврата фрейма.
    /// Не вызывается для not-returning syscall'ов (`thread_exit`).
    fn set_return(&mut self, value: i64);

    /// Записать дополнительное возвращаемое значение во второй
    /// регистр-возврата фрейма. Используется syscall'ами с парным
    /// результатом - сейчас [`SyscallOp::ChannelCreate`].
    fn set_secondary_return(&mut self, value: u64);

    /// Контекст, из которого был сделан вызов.
    fn origin(&self) -> Origin;
}

/// Главная точка входа: декодирует op, диспатчит на handler, кодирует
/// результат и записывает его в фрейм. Для not-returning syscall'ов
/// (`thread_exit`) функция не возвращается.
#[allow(clippy::too_many_lines)]
pub fn dispatch(frame: &mut dyn SyscallFrame) {
    if frame.origin() == Origin::Kernel {
        frame.set_return(SyscallError::KernelOriginated.into());
        return;
    }

    let op = match SyscallOp::from_raw(frame.op_raw()) {
        Ok(op) => op,
        Err(SyscallError::BadSyscall) => {
            frame.set_return(SyscallError::BadSyscall.into());
            return;
        }
        Err(e) => {
            frame.set_return(e.into());
            return;
        }
    };

    match op {
        SyscallOp::ObjectSignal => {
            let r = sys_object_signal(frame.arg(0), frame.arg(1), frame.arg(2));
            frame.set_return(encode_return(r));
        }
        SyscallOp::ObjectWaitOne => {
            let r = sys_object_wait_one(frame.arg(0), frame.arg(1), frame.arg(2));
            frame.set_return(encode_return(r));
        }
        SyscallOp::ObjectWaitMany => {
            sys_object_wait_many(frame);
        }
        SyscallOp::ChannelCreate => {
            sys_channel_create(frame);
        }
        SyscallOp::ChannelWrite => {
            let r = super::channel::sys_channel_write(
                frame.arg(0),
                frame.arg(1),
                frame.arg(2),
                frame.arg(3),
                frame.arg(4),
            );
            frame.set_return(encode_return(r));
        }
        SyscallOp::ChannelRead => {
            let r = super::channel::sys_channel_read(
                frame.arg(0),
                frame.arg(1),
                frame.arg(2),
                frame.arg(3),
                frame.arg(4),
            );
            frame.set_return(encode_return(r));
        }
        SyscallOp::HandleClose => {
            let r = sys_handle_close(frame.arg(0));
            frame.set_return(encode_return(r));
        }
        SyscallOp::HandleDuplicate => {
            let r = sys_handle_duplicate(frame.arg(0), frame.arg(1));
            frame.set_return(encode_return(r));
        }
        SyscallOp::ProcessCreate => {
            let r = super::process::sys_process_create(frame.arg(0), frame.arg(1));
            frame.set_return(encode_return(r));
        }
        SyscallOp::ProcessSelf => {
            let r = super::process::sys_process_self();
            frame.set_return(encode_return(r));
        }
        SyscallOp::ProcessLoadImage => {
            let r =
                super::process::sys_process_load_image(frame.arg(0), frame.arg(1), frame.arg(2));
            frame.set_return(encode_return(r));
        }
        SyscallOp::ProcessExitCode => {
            let r = super::process::sys_process_exit_code(frame.arg(0));
            frame.set_return(encode_return(r));
        }
        SyscallOp::ProcessTerminate => {
            let r = super::process::sys_process_terminate(frame.arg(0), frame.arg(1));
            frame.set_return(encode_return(r));
        }
        SyscallOp::ProcessStart => {
            let r = super::process::sys_process_start(
                frame.arg(0),
                frame.arg(1),
                frame.arg(2),
                frame.arg(3),
                frame.arg(4),
                frame.arg(5),
            );
            frame.set_return(encode_return(r));
        }
        SyscallOp::ThreadCreate => {
            let r = super::thread::sys_thread_create(
                frame.arg(0),
                frame.arg(1),
                frame.arg(2),
                frame.arg(3),
                frame.arg(4),
            );
            frame.set_return(encode_return(r));
        }
        SyscallOp::ThreadSelf => {
            let r = super::thread::sys_thread_self();
            frame.set_return(encode_return(r));
        }
        // Не возвращается: текущий поток помечается Terminated и
        // scheduler делает context switch на следующий.
        SyscallOp::ThreadExit => sys_thread_exit(frame.arg(0)),
        SyscallOp::ThreadExitCode => {
            let r = super::thread::sys_thread_exit_code(frame.arg(0));
            frame.set_return(encode_return(r));
        }
        SyscallOp::ThreadTerminate => {
            let r = super::thread::sys_thread_terminate(frame.arg(0), frame.arg(1));
            frame.set_return(encode_return(r));
        }
        SyscallOp::MemoryCreateVirtual => {
            let r = super::memory::sys_memory_create_virtual(frame.arg(0), frame.arg(1));
            frame.set_return(encode_return(r));
        }
        SyscallOp::MemoryCreatePhysical => {
            let r = super::memory::sys_memory_create_physical(
                frame.arg(0),
                frame.arg(1),
                frame.arg(2),
                frame.arg(3),
            );
            frame.set_return(encode_return(r));
        }
        SyscallOp::MemoryMap => {
            let r = super::memory::sys_memory_map(frame.arg(0), frame.arg(1), frame.arg(2));
            frame.set_return(encode_return(r));
        }
        SyscallOp::MemoryRemap => {
            let r = super::memory::sys_memory_remap(frame.arg(0), frame.arg(1), frame.arg(2));
            frame.set_return(encode_return(r));
        }
        SyscallOp::MemoryAllocate => {
            let r = super::memory::sys_memory_allocate(frame.arg(0), frame.arg(1));
            frame.set_return(encode_return(r));
        }
        SyscallOp::MemoryFree => {
            let r = super::memory::sys_memory_free(frame.arg(0), frame.arg(1));
            frame.set_return(encode_return(r));
        }
        SyscallOp::MemoryRegionInspect => {
            super::memory::sys_memory_region_inspect(frame);
        }
        SyscallOp::MailboxCreate => {
            let r = super::mailbox::sys_mailbox_create();
            frame.set_return(encode_return(r));
        }
        SyscallOp::MailboxQueue => {
            let r = super::mailbox::sys_mailbox_queue(frame.arg(0), frame.arg(1), frame.arg(2));
            frame.set_return(encode_return(r));
        }
        SyscallOp::MailboxWait => {
            let r = super::mailbox::sys_mailbox_wait(
                frame.arg(0),
                frame.arg(1),
                frame.arg(2),
                frame.arg(3),
            );
            frame.set_return(encode_return(r));
        }
        SyscallOp::MailboxWaitAsync => {
            let r = super::mailbox::sys_mailbox_wait_async(
                frame.arg(0),
                frame.arg(1),
                frame.arg(2),
                frame.arg(3),
            );
            frame.set_return(encode_return(r));
        }
        SyscallOp::MailboxCancel => {
            let r = super::mailbox::sys_mailbox_cancel(frame.arg(0), frame.arg(1), frame.arg(2));
            frame.set_return(encode_return(r));
        }
    }
}

/// `thread_exit(code)` - помечает текущий поток `Terminated` и
/// переключает на следующий ready-поток. Унифицирован с kernel-side
/// путём через `kobject::thread_exit`. Не возвращается.
fn sys_thread_exit(code: u64) -> ! {
    let exit_code = super::process::exit_code_from_arg(code);
    kobject::thread_exit(exit_code)
}

/// `object_signal(handle, set, clear)` - атомарно меняет биты сигналов
/// kernel-объекта. Возвращает `0` на успехе.
fn sys_object_signal(handle: u64, set: u64, clear: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    kobject::object_signal(id, signals_from_arg(set), signals_from_arg(clear))?;
    Ok(0)
}

/// `object_wait_one(handle, signals, timeout_ns)`. `timeout_ns == 0`
/// - poll. Возвращает observed-маску.
fn sys_object_wait_one(handle: u64, signals: u64, timeout_ns: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    let mask = signals_from_arg(signals);
    if mask == 0 {
        return Err(SyscallError::InvalidArgument);
    }
    let observed = kobject::object_wait_one(id, mask, Some(timeout_ns))?;
    Ok(u64::from(observed))
}

const WAIT_MANY_ENTRY_SIZE: usize = 8;
const WAIT_MANY_MAX_COUNT: usize = 256;

/// `object_wait_many(items_va, count, timeout_ns)`. Primary возврат -
/// observed-маска, secondary - индекс сработавшей записи.
fn sys_object_wait_many(frame: &mut dyn SyscallFrame) {
    match sys_object_wait_many_impl(frame.arg(0), frame.arg(1), frame.arg(2)) {
        Ok((index, observed)) => {
            frame.set_secondary_return(u64::from(index));
            frame.set_return(i64::from(observed));
        }
        Err(e) => frame.set_return(e.into()),
    }
}

fn sys_object_wait_many_impl(
    items_va: u64,
    count: u64,
    timeout_ns: u64,
) -> Result<(u32, u32), SyscallError> {
    let count = usize::try_from(count).map_err(|_| SyscallError::InvalidArgument)?;
    if count == 0 || count > WAIT_MANY_MAX_COUNT {
        return Err(SyscallError::InvalidArgument);
    }
    let bytes_total = count
        .checked_mul(WAIT_MANY_ENTRY_SIZE)
        .ok_or(SyscallError::InvalidArgument)?;
    validate_user_ptr(items_va, bytes_total)?;

    let user_vm = syscall_runtime()
        .current_user_vm()
        .ok_or(SyscallError::WrongType)?;

    let mut buf = [0u8; WAIT_MANY_MAX_COUNT * WAIT_MANY_ENTRY_SIZE];
    copy_in(&user_vm, items_va, &mut buf[..bytes_total])?;

    let mut items: [(HandleId, u32); WAIT_MANY_MAX_COUNT] = [(
        HandleId::from_raw(NonZeroU32::new(1).expect("non-zero literal")),
        0,
    ); WAIT_MANY_MAX_COUNT];
    for (i, chunk) in buf[..bytes_total]
        .chunks_exact(WAIT_MANY_ENTRY_SIZE)
        .enumerate()
    {
        let handle_raw = u32::from_le_bytes(chunk[0..4].try_into().expect("4 bytes per handle"));
        let mask = u32::from_le_bytes(chunk[4..8].try_into().expect("4 bytes per mask"));
        if mask == 0 {
            return Err(SyscallError::InvalidArgument);
        }
        let nz = NonZeroU32::new(handle_raw).ok_or(SyscallError::InvalidArgument)?;
        items[i] = (HandleId::from_raw(nz), mask);
    }

    let outcome = kobject::object_wait_many(&items[..count], Some(timeout_ns))?;
    let index = u32::try_from(outcome.index).expect("count <= WAIT_MANY_MAX_COUNT fits in u32");
    Ok((index, outcome.observed))
}

/// `channel_create()` - создаёт пару endpoint'ов и регистрирует оба
/// handle'а в текущей handle-table. На успехе записывает `left_id`
/// в основной регистр возврата, `right_id` - во вторичный.
/// Это позволяет вернуть пару `NonZeroU32` без ABI-конфликта с
/// отрицательным кодированием ошибок. Endpoint'ы симметричны -
/// любая сторона годится как "локальная".
fn sys_channel_create(frame: &mut dyn SyscallFrame) {
    match kobject::channel_create() {
        Ok((left_id, right_id)) => {
            frame.set_secondary_return(u64::from(right_id.raw().get()));
            frame.set_return(i64::from(left_id.raw().get()));
        }
        Err(e) => frame.set_return(SyscallError::from(e).into()),
    }
}

/// `handle_close(handle)` - изымает handle из таблицы и закрывает.
/// Возвращает `0` на успехе.
fn sys_handle_close(handle: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    kobject::handle_close(id)?;
    Ok(0)
}

/// `handle_duplicate(handle, new_rights)` - создаёт копию handle'а с
/// подмножеством прав. Возвращает сырой `HandleId` нового handle'а.
/// `new_rights` берётся из нижних 32 бит аргумента; неизвестные биты
/// отбрасываются [`Rights::from_bits_truncate`].
fn sys_handle_duplicate(handle: u64, new_rights: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    let rights_bits = u32::try_from(new_rights & u64::from(u32::MAX))
        .expect("masking guarantees value fits into u32");
    let rights = Rights::from_bits_truncate(rights_bits);
    let new_id = kobject::handle_duplicate(id, rights)?;
    Ok(u64::from(new_id.raw().get()))
}

/// 32-битная сигнальная маска из аргумента syscall'а. Верхние биты
/// игнорируются - ABI фиксирует, что биты сигналов живут в нижних 32-х.
fn signals_from_arg(raw: u64) -> u32 {
    u32::try_from(raw & u64::from(u32::MAX)).expect("masking guarantees value fits into u32")
}

/// Парсит ненулевой `HandleId` из аргумента syscall'а. `0`, а также
/// значения, не помещающиеся в `u32`, отвергаются как
/// [`SyscallError::InvalidArgument`].
pub(super) fn parse_handle_id(raw: u64) -> Result<kobject::HandleId, SyscallError> {
    let raw32 = u32::try_from(raw).map_err(|_| SyscallError::InvalidArgument)?;
    let nz = NonZeroU32::new(raw32).ok_or(SyscallError::InvalidArgument)?;
    Ok(kobject::HandleId::from_raw(nz))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Тестовая реализация [`SyscallFrame`] с настраиваемыми входами.
    pub(crate) struct MockFrame {
        pub op_raw: u16,
        pub args: [u64; 6],
        pub origin: Origin,
        pub returned: Option<i64>,
        pub secondary: Option<u64>,
    }

    impl MockFrame {
        pub fn user(op_raw: u16, args: [u64; 6]) -> Self {
            Self {
                op_raw,
                args,
                origin: Origin::User,
                returned: None,
                secondary: None,
            }
        }

        pub fn kernel(op_raw: u16, args: [u64; 6]) -> Self {
            Self {
                op_raw,
                args,
                origin: Origin::Kernel,
                returned: None,
                secondary: None,
            }
        }
    }

    impl SyscallFrame for MockFrame {
        fn op_raw(&self) -> u16 {
            self.op_raw
        }
        fn arg(&self, i: usize) -> u64 {
            self.args[i]
        }
        fn set_return(&mut self, value: i64) {
            self.returned = Some(value);
        }
        fn set_secondary_return(&mut self, value: u64) {
            self.secondary = Some(value);
        }
        fn origin(&self) -> Origin {
            self.origin
        }
    }

    #[test]
    fn unknown_op_returns_bad_syscall() {
        let mut f = MockFrame::user(0x55, [0; 6]);
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::BadSyscall)));
    }

    #[test]
    fn kernel_origin_is_rejected_before_dispatch() {
        let mut f = MockFrame::kernel(SyscallOp::ObjectSignal as u16, [1, 1, 0, 0, 0, 0]);
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::KernelOriginated)));
    }

    #[test]
    fn object_signal_with_zero_handle_is_invalid_argument() {
        let mut f = MockFrame::user(SyscallOp::ObjectSignal as u16, [0; 6]);
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::InvalidArgument)));
    }

    #[test]
    fn object_wait_one_with_zero_handle_is_invalid_argument() {
        let mut f = MockFrame::user(SyscallOp::ObjectWaitOne as u16, [0; 6]);
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::InvalidArgument)));
    }

    /// `signals == 0` бессмысленен даже для poll: ни один сигнал не
    /// сможет пересечься с пустой маской.
    #[test]
    fn object_wait_one_with_empty_mask_and_poll_timeout_is_invalid_argument() {
        let mut f = MockFrame::user(SyscallOp::ObjectWaitOne as u16, [1, 0, 0, 0, 0, 0]);
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::InvalidArgument)));
    }

    /// Пустую маску отвергаем и при ненулевом таймауте: wait всё равно
    /// никогда не пересечётся с сигналом, поэтому бессмысленен.
    #[test]
    fn object_wait_one_with_empty_mask_and_finite_timeout_is_invalid_argument() {
        let mut f = MockFrame::user(SyscallOp::ObjectWaitOne as u16, [1, 0, 1_000, 0, 0, 0]);
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::InvalidArgument)));
    }

    /// Только верхние 32 бита задают маску -> после `signals_from_arg`
    /// получим 0, что эквивалентно пустой маске.
    #[test]
    fn object_wait_one_with_only_upper_bits_is_invalid_argument() {
        let upper_only = u64::from(u32::MAX) + 1;
        let mut f = MockFrame::user(SyscallOp::ObjectWaitOne as u16, [1, upper_only, 0, 0, 0, 0]);
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::InvalidArgument)));
    }

    #[test]
    fn object_wait_many_with_zero_count_is_invalid_argument() {
        let mut f = MockFrame::user(SyscallOp::ObjectWaitMany as u16, [0x1000, 0, 0, 0, 0, 0]);
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::InvalidArgument)));
    }

    #[test]
    fn object_wait_many_with_overflow_count_is_invalid_argument() {
        let mut f = MockFrame::user(
            SyscallOp::ObjectWaitMany as u16,
            [0x1000, u64::MAX, 0, 0, 0, 0],
        );
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::InvalidArgument)));
    }

    #[test]
    fn object_wait_many_above_max_count_is_invalid_argument() {
        // Чуть выше потолка - ABI ограничивает массив.
        let above_max = (WAIT_MANY_MAX_COUNT + 1) as u64;
        let mut f = MockFrame::user(
            SyscallOp::ObjectWaitMany as u16,
            [0x1000, above_max, 0, 0, 0, 0],
        );
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::InvalidArgument)));
    }

    #[test]
    fn object_wait_many_with_zero_va_invalid_argument() {
        // count > 0, но va = 0 - validate_user_ptr отвергает.
        let mut f = MockFrame::user(SyscallOp::ObjectWaitMany as u16, [0, 1, 0, 0, 0, 0]);
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::InvalidArgument)));
    }

    #[test]
    fn parse_handle_id_zero_is_invalid_argument() {
        assert_eq!(parse_handle_id(0), Err(SyscallError::InvalidArgument));
    }

    #[test]
    fn parse_handle_id_overflow_is_invalid_argument() {
        let too_big = u64::from(u32::MAX) + 1;
        assert_eq!(parse_handle_id(too_big), Err(SyscallError::InvalidArgument));
    }

    #[test]
    fn parse_handle_id_nonzero_ok() {
        // `HandleId::from_raw` принимает любой `NonZeroU32`.
        let h = parse_handle_id(0x0001_0001).expect("non-zero handle id");
        assert_eq!(h.raw().get(), 0x0001_0001);
    }

    #[test]
    fn handle_close_with_zero_handle_is_invalid_argument() {
        let mut f = MockFrame::user(SyscallOp::HandleClose as u16, [0; 6]);
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::InvalidArgument)));
    }

    #[test]
    fn handle_duplicate_with_zero_handle_is_invalid_argument() {
        let mut f = MockFrame::user(SyscallOp::HandleDuplicate as u16, [0, 0, 0, 0, 0, 0]);
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::InvalidArgument)));
    }

    #[test]
    fn mailbox_queue_with_zero_handle_is_invalid_argument() {
        let mut f = MockFrame::user(SyscallOp::MailboxQueue as u16, [0, 0x1000, 32, 0, 0, 0]);
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::InvalidArgument)));
    }

    #[test]
    fn mailbox_wait_with_zero_handle_is_invalid_argument() {
        let mut f = MockFrame::user(SyscallOp::MailboxWait as u16, [0, 0, 0x1000, 32, 0, 0]);
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::InvalidArgument)));
    }

    #[test]
    fn mailbox_cancel_with_zero_handle_is_invalid_argument() {
        let mut f = MockFrame::user(SyscallOp::MailboxCancel as u16, [0, 1, 0, 0, 0, 0]);
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::InvalidArgument)));
    }

    #[test]
    fn mailbox_create_kernel_origin_rejected() {
        let mut f = MockFrame::kernel(SyscallOp::MailboxCreate as u16, [0; 6]);
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::KernelOriginated)));
    }
}
