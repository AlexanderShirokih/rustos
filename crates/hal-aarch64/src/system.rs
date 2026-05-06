//! Системные примитивы AArch64.

pub mod barrier {
    /// Выполняет полный системный барьер (DSB SY + ISB).
    pub fn full_system_barrier() {
        // SAFETY: `dsb sy`/`isb` - общесистемный барьер на ARMv8, не имеет операндов и
        // не модифицирует регистры/память (preserves_flags, nostack).
        unsafe { core::arch::asm!("dsb sy", "isb", options(nostack, preserves_flags)) };
    }
}
