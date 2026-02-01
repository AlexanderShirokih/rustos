pub mod barrier {
    /// Полный системный барьер
    pub fn full_system_barrier() {
        unsafe { core::arch::asm!("dsb sy", "isb", options(nostack, preserves_flags)) };
    }
}
