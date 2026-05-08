//! Архитектурно-независимая точка входа syscall-слоя.
//!
//! `SyscallFrame` - абстракция над регистровым контекстом, в которой
//! хранятся номер операции, аргументы и возвращаемое значение. Каждая
//! архитектура реализует этот trait для собственного trap-фрейма.
//!
//! ## Источник вызова
//!
//! [`Origin`] различает syscall'ы, инициированные из user-контекста и
//! из самого ядра. По дизайну kernel-side IPC должен ходить напрямую
//! через [`crate::kobject::api`], а не через trap, и в финальной
//! системе [`SyscallError::KernelOriginated`] будет возвращаться
//! ровно для [`Origin::Kernel`]. Сейчас, до активации user-режима,
//! диспатчер обрабатывает оба источника одинаково - это позволяет
//! покрывать syscall-слой end-to-end-тестами из kernel-thread'а;
//! [`SyscallFrame::origin`] уже доступен для будущей проверки.

use core::num::NonZeroU32;

use super::{
    error::{SyscallError, encode_return},
    numbers::SyscallOp,
};
use crate::{
    kobject::{self, Rights},
    syscall_bridge::scheduler,
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
pub fn dispatch(frame: &mut dyn SyscallFrame) {
    let op = match SyscallOp::from_raw(frame.op_raw()) {
        Ok(op) => op,
        Err(e) => {
            frame.set_return(e.into());
            return;
        }
    };

    match op {
        // Не возвращается: текущий поток помечается Terminated и
        // scheduler делает context switch на следующий.
        SyscallOp::ThreadExit => sys_thread_exit(frame.arg(0)),
        SyscallOp::ObjectSignal => {
            let r = sys_object_signal(frame.arg(0), frame.arg(1), frame.arg(2));
            frame.set_return(encode_return(r));
        }
        SyscallOp::ObjectWaitOne => {
            let r = sys_object_wait_one(frame.arg(0), frame.arg(1), frame.arg(2));
            frame.set_return(encode_return(r));
        }
        SyscallOp::ChannelCreate => {
            sys_channel_create(frame);
        }
        SyscallOp::HandleClose => {
            let r = sys_handle_close(frame.arg(0));
            frame.set_return(encode_return(r));
        }
        SyscallOp::HandleDuplicate => {
            let r = sys_handle_duplicate(frame.arg(0), frame.arg(1));
            frame.set_return(encode_return(r));
        }
        SyscallOp::MemoryAllocate => {
            let r = super::memory::sys_memory_allocate(frame.arg(0), frame.arg(1));
            frame.set_return(encode_return(r));
        }
        SyscallOp::MemoryRemap => {
            let r = super::memory::sys_memory_remap(frame.arg(0), frame.arg(1), frame.arg(2));
            frame.set_return(encode_return(r));
        }
        #[cfg(feature = "qemu-tests")]
        SyscallOp::TestEl0Probe => {
            crate::qemu_tests::el0_probe::record(frame.arg(0), frame.origin());
            sys_thread_exit(0);
        }
    }
}

/// `thread_exit(code)` - помечает текущий поток `Terminated` и
/// переключает на следующий ready-поток. Не возвращается.
///
/// Код выхода в текущей реализации игнорируется (нет места для его
/// хранения и потребителей), но включён в ABI для совместимости с
/// будущими `wait_for_thread`-семантиками.
fn sys_thread_exit(_code: u64) -> ! {
    scheduler().exit()
}

/// `object_signal(handle, set, clear)` - атомарно меняет биты сигналов
/// kernel-объекта. Возвращает `0` на успехе.
fn sys_object_signal(handle: u64, set: u64, clear: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    kobject::object_signal(id, signals_from_arg(set), signals_from_arg(clear))?;
    Ok(0)
}

/// `object_wait_one(handle, signals, timeout_ns)` - ждёт хотя бы один
/// бит из `signals` на kernel-объекте. `timeout_ns == 0` означает
/// бессрочный wait; ненулевое значение - длительность дедлайна в
/// наносекундах (относительно текущего момента). Возвращает наблюдённую
/// маску.
fn sys_object_wait_one(handle: u64, signals: u64, timeout_ns: u64) -> Result<u64, SyscallError> {
    let id = parse_handle_id(handle)?;
    let mask = signals_from_arg(signals);
    if mask == 0 {
        return Err(SyscallError::InvalidArgument);
    }
    let timeout = if timeout_ns == 0 {
        None
    } else {
        Some(timeout_ns)
    };
    let observed = kobject::object_wait_one(id, mask, timeout)?;
    Ok(u64::from(observed))
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
fn parse_handle_id(raw: u64) -> Result<kobject::HandleId, SyscallError> {
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
        let mut f = MockFrame::user(99, [0; 6]);
        dispatch(&mut f);
        assert_eq!(f.returned, Some(i64::from(SyscallError::BadSyscall)));
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

    /// `signals == 0` с бессрочным `timeout_ns == 0` припарковал бы
    /// поток навсегда - отвергаем до того, как wait дойдёт до kobject.
    #[test]
    fn object_wait_one_with_empty_mask_and_no_timeout_is_invalid_argument() {
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
}
