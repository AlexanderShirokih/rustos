/// dsb sy упорядочивает device-деассерт до re-enable level-линии.
pub(crate) fn barrier_before_unmask() {
    // SAFETY: барьер без операндов и доступа к памяти.
    unsafe { core::arch::asm!("dsb sy", options(nostack, preserves_flags)) };
}
