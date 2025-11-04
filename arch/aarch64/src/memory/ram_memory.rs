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
    #[inline(always)]
    fn frame_size(&self) -> usize {
        self.frame_size
    }

    #[inline(always)]
    fn read_bytes(&self, addr: PhysicalAddress, buf: &mut [u8]) {
        unsafe {
            asm!("dsb ish", options(nostack, preserves_flags));
            let src = self.to_ptr(addr) as *const u8;
            ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), buf.len());
            asm!("dsb ish", options(nostack, preserves_flags));
        };
    }

    #[inline(always)]
    fn write_bytes(&self, addr: PhysicalAddress, buf: &[u8]) {
        let src = self.to_ptr(addr);
        unsafe { ptr::copy_nonoverlapping(buf.as_ptr(), src, buf.len()) };

        self.clean_page_cache(addr);
    }

    fn enable_virtual_mode(&self, root_page: PhysicalAddress) {
        unsafe {
            // 1) Барьер перед изменениями регистров
            asm!("dsb ish; isb", options(nostack, preserves_flags));

            // 2) Записываем корень таблицы
            let ttbr0 = root_page.as_usize() as u64;
            asm!("msr ttbr0_el1, {}", in(reg) ttbr0);

            // 3) MAIR_EL1: Attr0=WB, Attr1=Device
            let mair = (0xffu64 << 0) | (0x04u64 << 8);
            asm!("msr mair_el1, {}", in(reg) mair);

            // 4) TCR_EL1: 48-bit VA, inner-WB, shareable, TG0=4K
            let tcr = (16u64 << 0)    // T0SZ = 64-48
                | (0b01 << 8)    // IRGN0
                | (0b01 << 10)   // ORGN0
                | (1 << 12)      // SH0 = Inner
                | (0 << 14); // TG0 = 4K
            asm!("msr tcr_el1, {}", in(reg) tcr);

            // 5) Сброс TLB
            asm!(
                "dsb ish; tlbi vmalle1; dsb ish; isb",
                options(nostack, preserves_flags)
            );

            // 6) Включаем MMU + кэши
            let sctlr_flags: u64 = (1 << 0) | (1 << 2) | (1 << 12);
            asm!(
                "mrs x0, sctlr_el1",
                "orr x0, x0, {flags}",
                "msr sctlr_el1, x0",
                "dsb ish; isb",
            flags = in(reg) sctlr_flags,
            options(nostack, preserves_flags)
            );
        }
    }

    #[inline(always)]
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

    #[inline(always)]
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
