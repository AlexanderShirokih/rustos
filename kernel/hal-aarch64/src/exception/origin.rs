const SPSR_M_MASK: u64 = 0xF;

pub(crate) const fn originates_from_el0(spsr: u64) -> bool {
    spsr & SPSR_M_MASK == 0
}
