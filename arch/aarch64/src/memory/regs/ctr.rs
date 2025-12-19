//! Cache Type Register
//! Регистр, который описывает устройство кэшей CPU
//!

use crate::memory::regs::common::EL0;

pub struct CacheTypeRegister<EL> {
    _phantom: core::marker::PhantomData<EL>,
}

impl CacheTypeRegister<EL0> {
    pub const fn new() -> Self {
        Self {
            _phantom: core::marker::PhantomData,
        }
    }

    pub fn get_line_size(&self) -> usize {
        let ctr = Self::get_raw();
        let dminline = ((ctr >> 16) & 0xF) as usize;

        4usize << dminline
    }

    fn get_raw() -> u64 {
        let ctr: u64;
        unsafe { core::arch::asm!("mrs {}, ctr_el0", out(reg) ctr) }

        ctr
    }
}
