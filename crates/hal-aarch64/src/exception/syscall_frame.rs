//! Реализация [`main::syscall::SyscallFrame`] для aarch64-фрейма
//! исключения.
//!
//! Связь регистров с syscall ABI:
//! - immediate инструкции `SVC #N` лежит в `ESR_EL1.ISS[15:0]`;
//! - аргументы `arg(0)..arg(5)` - это `x0..x5` (AAPCS64);
//! - возврат записывается в `x0` фрейма; assembly `exception_entry!`
//!   восстанавливает его из фрейма перед `eret`;
//! - источник трапа (EL0 vs EL1) определяется по `SPSR_EL1.M[3:0]`:
//!   `0` - EL0t, иное - EL1.

use main::syscall::{Origin, SyscallFrame};

use super::exceptions::ExceptionFrame;

const ARG_COUNT: usize = 6;
/// Биты M[3:0] в SPSR; `0` соответствует EL0t (user mode).
const SPSR_M_MASK: u64 = 0xF;

impl SyscallFrame for ExceptionFrame {
    fn op_raw(&self) -> u16 {
        (self.esr.iss() & 0xFFFF) as u16
    }

    fn arg(&self, i: usize) -> u64 {
        debug_assert!(i < ARG_COUNT, "syscall arg index out of range: {i}");
        u64::from(self.regs[i])
    }

    fn set_return(&mut self, value: i64) {
        // Битовое представление i64 как u64 - требование ABI, знак сохраняется.
        self.regs[0] = value.cast_unsigned().into();
    }

    fn set_secondary_return(&mut self, value: u64) {
        self.regs[1] = value.into();
    }

    fn origin(&self) -> Origin {
        if (self.spsr & SPSR_M_MASK) == 0 {
            Origin::User
        } else {
            Origin::Kernel
        }
    }
}
