//! Системные примитивы AArch64.

pub mod barrier {
    /// Выполняет полный системный барьер (DSB SY + ISB).
    pub fn full_system_barrier() {
        unsafe { core::arch::asm!("dsb sy", "isb", options(nostack, preserves_flags)) };
    }
}
