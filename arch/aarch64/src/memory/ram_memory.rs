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
        addr.0 as *mut u8
    }
}

impl MemoryBackend for Aarch64RamMemory {
    fn frame_size(&self) -> usize {
        self.frame_size
    }

    #[inline]
    fn read<T>(&self, addr: PhysicalAddress) -> T {
        unsafe { ptr::read_unaligned(self.to_ptr(addr) as *const T) }
    }

    #[inline]
    fn write<T: Copy>(&self, addr: PhysicalAddress, val: T) {
        unsafe { ptr::write_unaligned(self.to_ptr(addr) as *mut T, val) }
    }

    fn clean_page_cache(&self, address: PhysicalAddress) {
        unsafe {
            let cache_line_size = 64; // Should be queried from CTR_EL0
            let mut addr = address.as_usize();
            let end_addr = addr + self.frame_size;

            while addr < end_addr {
                asm!("dc cvau, {}", in(reg) addr, options(nostack, preserves_flags));
                addr += cache_line_size;
            }

            addr = address.as_usize();
            while addr < end_addr {
                asm!("ic ivau, {}", in(reg) addr, options(nostack, preserves_flags));
                addr += cache_line_size;
            }

            asm!("dsb ish; isb", options(nostack, preserves_flags));
        }
    }

    fn invalidate_cache(&self) {
        unsafe {
            asm!("dsb ish", options(nostack, preserves_flags));

            // 2. Инвалидировать все TLB записи для EL1 и EL0
            asm!("tlbi vmalle1", options(nostack, preserves_flags));

            // 3. Дождаться завершения TLB инвалидации
            asm!("dsb ish", options(nostack, preserves_flags));

            // 4. Синхронизировать конвейер инструкций
            asm!("isb", options(nostack, preserves_flags));
        }
    }
}
