pub mod barrier {
    /// Барьер для синхронизации операций памяти (Inner Shareable)
    pub fn barrier() {
        unsafe { core::arch::asm!("dsb ish", "isb", options(nostack, preserves_flags)) };
    }

    /// Полный системный барьер — нужен перед включением MMU
    pub fn full_system_barrier() {
        unsafe { core::arch::asm!("dsb sy", "isb", options(nostack, preserves_flags)) };
    }
}
