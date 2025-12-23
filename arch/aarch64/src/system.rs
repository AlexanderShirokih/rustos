pub mod barrier {
    pub fn barrier() {
        unsafe { core::arch::asm!("dsb ish", "isb", options(nostack, preserves_flags)) };
    }
}
