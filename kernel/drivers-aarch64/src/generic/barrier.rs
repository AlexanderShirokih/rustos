pub(crate) fn barrier_before_unmask() {
    // dsb sy упорядочивает device-деассерт до re-enable level-линии.
    // SAFETY: барьер без операндов и доступа к памяти.
    unsafe { core::arch::asm!("dsb sy", options(nostack, preserves_flags)) };
}
