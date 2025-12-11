pub mod barrier {
    #[inline(always)]
    pub fn barrier() {
        unsafe { core::arch::asm!("dsb ish", "isb", options(nostack, preserves_flags)) };
    }
}

pub mod interrupts {
    #[inline(always)]
    pub fn mask() {
        unsafe {
            // Маскируем Debug, SError, IRQ, FIQ.
            core::arch::asm!(
                "msr daifset, #0xf",
                "isb",
                options(nostack, preserves_flags)
            )
        };
    }
}
