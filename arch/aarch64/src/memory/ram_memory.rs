use core::arch::asm;
use core::ptr;
use memory::memory_backend::MemoryBackend;
use memory::physical::PhysicalAddress;

#[derive(Copy, Clone)]
pub struct Aarch64RamMemory {
    frame_size: usize,
}

impl Aarch64RamMemory {
    pub const fn new(frame_size: usize) -> Self {
        Self { frame_size }
    }

    #[inline]
    fn to_ptr(&self, addr: PhysicalAddress) -> *mut u8 {
        (addr.0) as *mut u8
    }
}

impl MemoryBackend for Aarch64RamMemory {
    fn frame_size(&self) -> usize {
        self.frame_size
    }

    fn read_bytes(&self, addr: PhysicalAddress, buf: &mut [u8]) {
        let src = self.to_ptr(addr) as *const u8;
        unsafe { ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), buf.len()) };
    }

    fn write_bytes(&self, addr: PhysicalAddress, buf: &[u8]) {
        let src = self.to_ptr(addr);
        unsafe { ptr::copy_nonoverlapping(buf.as_ptr(), src, buf.len()) };
    }

    fn set_virtual_mode_enabled(&self, enabled: bool, root_page: PhysicalAddress) {
        unsafe {
            // загрузить новый корневой указатель
            asm!("msr ttbr0_el1, {}", in(reg) root_page.0);
            asm!("isb");

            // прочитать и модифицировать SCTLR_EL1.M (bit0)
            let mut sctlr: u64;
            asm!("mrs {}, sctlr_el1", out(reg) sctlr);
            if enabled {
                sctlr |= 1; // включить трансляцию
            } else {
                sctlr &= !1; // выключить
            }
            asm!("msr sctlr_el1, {}", in(reg) sctlr);
            asm!("isb");
        }
    }
}
