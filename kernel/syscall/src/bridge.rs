//! Архитектурно-независимая точка входа syscall-слоя: трейт `SyscallFrame`
//! над регистровым контекстом и [`dispatch`]. Trap из [`Origin::Kernel`]
//! отвергается с [`SyscallError::KernelOriginated`] до диспатча.

use core::num::NonZeroU32;

use kobject::{self, HandleId, Rights};

use super::{
    error::{SyscallError, encode_return},
    numbers::{SyscallOp, op_from_raw},
    runtime::runtime as syscall_runtime,
    user_io::{copy_in, validate_user_ptr},
};

/// Источник syscall-вызова.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    User,
    Kernel,
}

/// Доступ к регистровому контексту syscall-вызова. Реализуется
/// платформенным слоем для своего trap-фрейма.
pub trait SyscallFrame {
    fn op_raw(&self) -> u16;

    /// Аргумент `i` (0..=5). Для `i >= 6` поведение не определено.
    fn arg(&self, i: usize) -> u64;

    fn set_return(&mut self, value: i64);

    /// Записать дополнительное возвращаемое значение во второй
    /// регистр-возврата фрейма. Используется syscall'ами с парным
    /// результатом - сейчас [`SyscallOp::SignalWaitMany`].
    fn set_secondary_return(&mut self, value: u64);

    fn origin(&self) -> Origin;
}

/// Главная точка входа: декодирует op, диспатчит на handler, кодирует результат.
/// Для `ThreadExit` не возвращается.
#[allow(clippy::too_many_lines)]
pub fn dispatch(frame: &mut dyn SyscallFrame) {
    if frame.origin() == Origin::Kernel {
        frame.set_return(SyscallError::KernelOriginated.into());
        return;
    }

    let op = match op_from_raw(frame.op_raw()) {
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
        SyscallOp::SignalSet => {
            let r = sys_signal_set(frame.arg(0), frame.arg(1), frame.arg(2), frame.arg(3));
            frame.set_return(encode_return(r));
        }
        SyscallOp::SignalCreate => {
            let r = sys_signal_create();
            frame.set_return(encode_return(r));
        }
        SyscallOp::IpcBufferAddr => {
            let r = super::thread::sys_ipc_buffer_addr();
            frame.set_return(encode_return(r));
        }
        SyscallOp::SignalWaitOne => {
            let r = sys_signal_wait_one(frame.arg(0), frame.arg(1), frame.arg(2));
            frame.set_return(encode_return(r));
        }
        SyscallOp::SignalWaitMany => {
            sys_signal_wait_many(frame);
        }
        SyscallOp::PortCreate => {
            let r = super::port::sys_port_create();
            frame.set_return(encode_return(r));
        }
        SyscallOp::PortSend => {
            let r = super::port::sys_port_send(frame.arg(0), frame.arg(1));
            frame.set_return(encode_return(r));
        }
        SyscallOp::PortRecv => {
            let r = super::port::sys_port_recv(frame.arg(0), frame.arg(1));
            frame.set_return(encode_return(r));
        }
        SyscallOp::PortCall => {
            let r = super::port::sys_port_call(frame.arg(0), frame.arg(1));
            frame.set_return(encode_return(r));
        }
        SyscallOp::PortReply => {
            let r = super::port::sys_port_reply(frame.arg(0));
            frame.set_return(encode_return(r));
        }
        SyscallOp::HandleClose => {
            let r = sys_handle_close(frame.arg(0));
            frame.set_return(encode_return(r));
        }
        SyscallOp::HandleDuplicate => {
            let r = sys_handle_duplicate(frame.arg(0), frame.arg(1), frame.arg(2));
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
        SyscallOp::ProcessTerminationSignal => {
            let r = super::process::sys_process_termination_signal(frame.arg(0));
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
        // планировщик делает context switch на следующий.
        SyscallOp::ThreadExit => sys_thread_exit(frame.arg(0)),
        SyscallOp::ThreadExitCode => {
            let r = super::thread::sys_thread_exit_code(frame.arg(0));
            frame.set_return(encode_return(r));
        }
        SyscallOp::ThreadTerminate => {
            let r = super::thread::sys_thread_terminate(frame.arg(0), frame.arg(1));
            frame.set_return(encode_return(r));
        }
        SyscallOp::ThreadTerminationSignal => {
            let r = super::thread::sys_thread_termination_signal(frame.arg(0));
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
    }
}

/// Помечает текущий поток `Terminated` и делает context switch. Не возвращается.
fn sys_thread_exit(code: u64) -> ! {
    let exit_code = super::process::exit_code_from_arg(code);
    kobject::thread_exit(exit_code)
}

/// `signal_set(handle, set, clear, count)` - атомарно меняет биты
/// сигналов kernel-объекта и будит waiter'ов.
/// При `count == 0` будит всех пересекающихся.
/// При `count == N` - не более N в FIFO-порядке.
/// Возвращает `0` в случае успеха.
fn sys_signal_set(handle: u64, set: u64, clear: u64, count: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    let count =
        u32::try_from(count & u64::from(u32::MAX)).expect("masking guarantees value fits into u32");
    kobject::signal_set(id, signals_from_arg(set), signals_from_arg(clear), count)?;
    Ok(0)
}

/// Создаёт `Signal` и возвращает его сырой `HandleId`.
fn sys_signal_create() -> Result<u64, SyscallError> {
    let id = kobject::signal_create()?;
    Ok(u64::from(id.raw().get()))
}

/// `timeout_ns == 0` - poll. Возвращает observed-маску.
fn sys_signal_wait_one(handle: u64, signals: u64, timeout_ns: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    let mask = signals_from_arg(signals);
    if mask == 0 {
        return Err(SyscallError::InvalidArgument);
    }
    let observed = kobject::signal_wait_one(id, mask, Some(timeout_ns))?;
    Ok(u64::from(observed))
}

const WAIT_MANY_ENTRY_SIZE: usize = 8;
const WAIT_MANY_MAX_COUNT: usize = 256;

/// Primary возврат - observed-маска, secondary - индекс сработавшей записи.
fn sys_signal_wait_many(frame: &mut dyn SyscallFrame) {
    match sys_signal_wait_many_impl(frame.arg(0), frame.arg(1), frame.arg(2)) {
        Ok((index, observed)) => {
            frame.set_secondary_return(u64::from(index));
            frame.set_return(i64::from(observed));
        }
        Err(e) => frame.set_return(e.into()),
    }
}

fn sys_signal_wait_many_impl(
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

    let outcome = kobject::signal_wait_many(&items[..count], Some(timeout_ns))?;
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

/// Изымает handle из таблицы и закрывает.
fn sys_handle_close(handle: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    kobject::handle_close(id)?;
    Ok(0)
}

/// Создаёт копию handle'а с подмножеством прав. `new_rights` - нижние 32 бита arg1,
/// неизвестные биты отбрасываются. `badge` - set-once: переклеймить уже
/// заклеймённый -> `BadHandle`.
fn sys_handle_duplicate(handle: u64, new_rights: u64, badge: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    let rights_bits = u32::try_from(new_rights & u64::from(u32::MAX))
        .expect("masking guarantees value fits into u32");
    let rights = Rights::from_bits_truncate(rights_bits);
    let new_id = kobject::handle_duplicate(id, rights, badge)?;
    Ok(u64::from(new_id.raw().get()))
}

/// Верхние биты игнорируются: ABI фиксирует биты сигналов в нижних 32.
fn signals_from_arg(raw: u64) -> u32 {
    u32::try_from(raw & u64::from(u32::MAX)).expect("masking guarantees value fits into u32")
}

/// `0` и значения > `u32::MAX` - `InvalidArgument`.
pub(super) fn parse_handle_id(raw: u64) -> Result<kobject::HandleId, SyscallError> {
    let raw32 = u32::try_from(raw).map_err(|_| SyscallError::InvalidArgument)?;
    let nz = NonZeroU32::new(raw32).ok_or(SyscallError::InvalidArgument)?;
    Ok(kobject::HandleId::from_raw(nz))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut f = MockFrame::user(0x57, [0; 6]);
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::BadSyscall)));
    }

    #[test]
    fn kernel_origin_is_rejected_before_dispatch() {
        let mut f = MockFrame::kernel(SyscallOp::SignalSet as u16, [1, 1, 0, 0, 0, 0]);
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::KernelOriginated)));
    }

    #[test]
    fn signal_set_with_zero_handle_is_invalid_argument() {
        let mut f = MockFrame::user(SyscallOp::SignalSet as u16, [0; 6]);
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::InvalidArgument)));
    }

    #[test]
    fn signal_wait_one_with_zero_handle_is_invalid_argument() {
        let mut f = MockFrame::user(SyscallOp::SignalWaitOne as u16, [0; 6]);
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::InvalidArgument)));
    }

    #[test]
    fn signal_wait_one_with_empty_mask_and_poll_timeout_is_invalid_argument() {
        let mut f = MockFrame::user(SyscallOp::SignalWaitOne as u16, [1, 0, 0, 0, 0, 0]);
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::InvalidArgument)));
    }

    #[test]
    fn signal_wait_one_with_empty_mask_and_finite_timeout_is_invalid_argument() {
        let mut f = MockFrame::user(SyscallOp::SignalWaitOne as u16, [1, 0, 1_000, 0, 0, 0]);
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::InvalidArgument)));
    }

    #[test]
    fn signal_wait_one_with_only_upper_bits_is_invalid_argument() {
        let upper_only = u64::from(u32::MAX) + 1;
        let mut f = MockFrame::user(SyscallOp::SignalWaitOne as u16, [1, upper_only, 0, 0, 0, 0]);
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::InvalidArgument)));
    }

    #[test]
    fn signal_wait_many_with_zero_count_is_invalid_argument() {
        let mut f = MockFrame::user(SyscallOp::SignalWaitMany as u16, [0x1000, 0, 0, 0, 0, 0]);
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::InvalidArgument)));
    }

    #[test]
    fn signal_wait_many_with_overflow_count_is_invalid_argument() {
        let mut f = MockFrame::user(
            SyscallOp::SignalWaitMany as u16,
            [0x1000, u64::MAX, 0, 0, 0, 0],
        );
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::InvalidArgument)));
    }

    #[test]
    fn signal_wait_many_above_max_count_is_invalid_argument() {
        let above_max = (WAIT_MANY_MAX_COUNT + 1) as u64;
        let mut f = MockFrame::user(
            SyscallOp::SignalWaitMany as u16,
            [0x1000, above_max, 0, 0, 0, 0],
        );
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::InvalidArgument)));
    }

    #[test]
    fn signal_wait_many_with_zero_va_invalid_argument() {
        let mut f = MockFrame::user(SyscallOp::SignalWaitMany as u16, [0, 1, 0, 0, 0, 0]);
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
    fn raw_op_0x13_routes_to_signal_create() {
        assert_eq!(op_from_raw(0x13), Ok(SyscallOp::SignalCreate));
    }

    #[test]
    fn signal_create_kernel_origin_rejected() {
        let mut f = MockFrame::kernel(SyscallOp::SignalCreate as u16, [0; 6]);
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::KernelOriginated)));
    }
}
