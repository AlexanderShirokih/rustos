use crate::mem_flags::{Access, MemFlags, Shareability};

pub struct KernelText;
pub struct KernelData;
pub struct KernelRoData;
pub struct Mmio;

impl KernelText {
    pub const fn flags() -> MemFlags {
        MemFlags::new()
            .af(true)
            .sh(Shareability::Inner)
            .ap(Access::KernelRW)
            .attr_index(0) // Normal WB
            .pxn(false)
            .uxn(true)
    }
}

impl KernelData {
    pub const fn flags() -> MemFlags {
        MemFlags::new()
            .af(true)
            .sh(Shareability::Inner)
            .ap(Access::KernelRW)
            .attr_index(0)
            .pxn(true)
            .uxn(true)
    }
}

impl KernelRoData {
    pub const fn flags() -> MemFlags {
        MemFlags::new()
            .af(true)
            .sh(Shareability::Inner)
            .ap(Access::KernelRO)
            .attr_index(0) // Normal WB
            .pxn(false)
            .uxn(true)
    }
}

impl Mmio {
    pub const fn flags() -> MemFlags {
        MemFlags::new()
            .af(true)
            .sh(Shareability::None)
            .ap(Access::KernelRW)
            .attr_index(1) // Device-nGnRE
            .pxn(true)
            .uxn(true)
    }
}
