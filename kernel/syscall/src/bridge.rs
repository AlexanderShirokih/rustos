//! Архитектурно-независимая точка входа syscall-слоя: трейт `SyscallFrame`
//! над регистровым контекстом и [`dispatch`]. Trap из [`Origin::Kernel`]
//! отвергается с [`SyscallError::KernelOriginated`] до диспатча.

use core::num::NonZeroU32;

use capability::{self, HandleId, Rights};
use syscall::{SyscallError, WAIT_MANY_MAX_COUNT, WakeCount};

use super::{
    error::{encode_return, map_ipc_error},
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

    /// Записать второй возвращаемый регистр (парный результат).
    fn set_secondary_return(&mut self, value: u64);

    fn origin(&self) -> Origin;
}

/// Главная точка входа декодирует op, диспатчит на handler и кодирует результат.
/// Для `ThreadExit` и `ProcessExit` не возвращается. Поток снимаемого процесса возвращается
/// отсюда и завершается на границе возврата в EL0.
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
        SyscallOp::ThreadIpcBufferAddr => {
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
        SyscallOp::PortCallAsync | SyscallOp::PortSendAsync => {
            let r = super::port::sys_port_async(frame.arg(0), op == SyscallOp::PortCallAsync);
            frame.set_return(encode_return(r));
        }
        SyscallOp::PendingTake => {
            let r = super::port::sys_pending_take(frame.arg(0));
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
        SyscallOp::ProcessResourceSelf => {
            let r = super::process::sys_process_resource_self();
            frame.set_return(encode_return(r));
        }
        SyscallOp::ResourceSplit => {
            let r = super::process::sys_resource_split(frame.arg(0), frame.arg(1));
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
        SyscallOp::ProcessExit => sys_process_exit(frame.arg(0)),
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
        // Не возвращается. Текущий поток помечается Terminated и
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
        SyscallOp::ThreadYield => {
            capability::thread_yield();
            frame.set_return(0);
        }
        SyscallOp::MemoryCreateVirtual => {
            let r = super::memory::sys_memory_create_virtual(
                frame.arg(0),
                frame.arg(1),
                frame.arg(2),
                frame.arg(3),
            );
            frame.set_return(encode_return(r));
        }
        SyscallOp::MemorySlice => {
            let r = super::memory::sys_memory_slice(
                frame.arg(0),
                frame.arg(1),
                frame.arg(2),
                frame.arg(3),
                frame.arg(4),
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
            let r = super::memory::sys_memory_allocate(frame.arg(0), frame.arg(1), frame.arg(2));
            frame.set_return(encode_return(r));
        }
        SyscallOp::MemoryFree => {
            let r = super::memory::sys_memory_free(frame.arg(0), frame.arg(1));
            frame.set_return(encode_return(r));
        }
        SyscallOp::MemoryRegionInspect => {
            super::memory::sys_memory_region_inspect(frame);
        }
        SyscallOp::IrqMint => {
            let r = super::irq::sys_irq_mint(frame.arg(0), frame.arg(1), frame.arg(2));
            frame.set_return(encode_return(r));
        }
        SyscallOp::IrqAck => {
            let r = super::irq::sys_irq_ack(frame.arg(0));
            frame.set_return(encode_return(r));
        }
    }
}

fn sys_thread_exit(code: u64) -> ! {
    let exit_code = super::process::exit_code_from_arg(code);
    capability::thread_exit(exit_code)
}

fn sys_process_exit(code: u64) -> ! {
    let exit_code = super::process::exit_code_from_arg(code);
    capability::process_exit(exit_code)
}

fn sys_signal_set(handle: u64, set: u64, clear: u64, count: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    let count = WakeCount::from_raw(count).ok_or(SyscallError::InvalidArgument)?;
    capability::signal_set(id, signals_from_arg(set), signals_from_arg(clear), count)
        .map_err(map_ipc_error)?;
    Ok(0)
}

fn sys_signal_create() -> Result<u64, SyscallError> {
    let id = capability::signal_create().map_err(map_ipc_error)?;
    Ok(u64::from(id.raw().get()))
}

fn sys_signal_wait_one(handle: u64, signals: u64, timeout_ns: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    let mask = signals_from_arg(signals);
    if mask == 0 {
        return Err(SyscallError::InvalidArgument);
    }
    let observed =
        capability::signal_wait_one(id, mask, Some(timeout_ns)).map_err(map_ipc_error)?;
    Ok(u64::from(observed))
}

const WAIT_MANY_ENTRY_SIZE: usize = 8;

const WAIT_MANY_PLACEHOLDER: (HandleId, u32) = (HandleId::from_raw(NonZeroU32::MIN), 0);

fn parse_wait_many_entry(
    entry: [u8; WAIT_MANY_ENTRY_SIZE],
) -> Result<(HandleId, u32), SyscallError> {
    let [h0, h1, h2, h3, m0, m1, m2, m3] = entry;
    let mask = u32::from_le_bytes([m0, m1, m2, m3]);
    if mask == 0 {
        return Err(SyscallError::InvalidArgument);
    }
    let handle = NonZeroU32::new(u32::from_le_bytes([h0, h1, h2, h3]))
        .ok_or(SyscallError::InvalidArgument)?;
    Ok((HandleId::from_raw(handle), mask))
}

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
    let target = buf
        .get_mut(..bytes_total)
        .ok_or(SyscallError::InvalidArgument)?;
    copy_in(&user_vm, items_va, target)?;

    // Хвост `items` за `count` наружу не уходит (`&items[..count]`).
    let mut items: [(HandleId, u32); WAIT_MANY_MAX_COUNT] =
        [WAIT_MANY_PLACEHOLDER; WAIT_MANY_MAX_COUNT];
    let (entries, _) = buf
        .get(..bytes_total)
        .ok_or(SyscallError::InvalidArgument)?
        .as_chunks::<WAIT_MANY_ENTRY_SIZE>();
    for (slot, entry) in items.iter_mut().zip(entries) {
        *slot = parse_wait_many_entry(*entry)?;
    }

    let waited = items.get(..count).ok_or(SyscallError::InvalidArgument)?;
    let outcome = capability::signal_wait_many(waited, Some(timeout_ns)).map_err(map_ipc_error)?;
    Ok((outcome.index, outcome.observed))
}

fn sys_handle_close(handle: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    capability::handle_close(id).map_err(map_ipc_error)?;
    Ok(0)
}

fn sys_handle_duplicate(handle: u64, new_rights: u64, badge: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    let rights = Rights::from_bits_truncate(low_u32(new_rights));
    let new_id = capability::handle_duplicate(id, rights, badge).map_err(map_ipc_error)?;
    Ok(u64::from(new_id.raw().get()))
}

#[allow(clippy::cast_possible_truncation)]
const fn low_u32(raw: u64) -> u32 {
    // Поля вызова занимают младшее слово аргумента, и старшая половина в них не участвует.
    raw as u32
}

fn signals_from_arg(raw: u64) -> u32 {
    low_u32(raw)
}

pub(super) fn parse_handle_id(raw: u64) -> Result<capability::HandleId, SyscallError> {
    let raw32 = u32::try_from(raw).map_err(|_| SyscallError::InvalidArgument)?;
    let nz = NonZeroU32::new(raw32).ok_or(SyscallError::InvalidArgument)?;
    Ok(capability::HandleId::from_raw(nz))
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
        pub(crate) fn user(op_raw: u16, args: [u64; 6]) -> Self {
            Self {
                op_raw,
                args,
                origin: Origin::User,
                returned: None,
                secondary: None,
            }
        }

        pub(crate) fn kernel(op_raw: u16, args: [u64; 6]) -> Self {
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
