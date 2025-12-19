use crate::memory::regs::ctr;
use crate::system;
use core::arch::asm;
use core::mem::size_of;
use core::ptr;
use memory::aligned::{Address, Aligned};
use memory::memory::MemoryAccessProvider;
use memory::virtual_address::{AlignedVirtualAddress, VirtualAddress};

pub struct Aarch64VirtualRamMemory;

impl Aarch64VirtualRamMemory {
    pub const fn new() -> Self {
        Self
    }

    #[inline]
    fn to_ptr(&self, addr: VirtualAddress) -> *mut u8 {
        addr.as_u64() as *mut u8
    }
}

impl MemoryAccessProvider for Aarch64VirtualRamMemory {
    #[inline]
    fn read<T>(&self, addr: VirtualAddress) -> T {
        unsafe { ptr::read_unaligned(self.to_ptr(addr) as *const T) }
    }

    #[inline]
    fn write<T>(&self, addr: VirtualAddress, val: &T) {
        unsafe {
            ptr::copy_nonoverlapping(
                val as *const T as *const u8,
                self.to_ptr(addr),
                size_of::<T>(),
            )
        }
    }

    fn clean_cache<const SHIFT: u8>(&self, address: AlignedVirtualAddress<SHIFT>) {
        unsafe {
            let ctr = ctr::CacheTypeRegister::new();
            let cache_line_size = ctr.get_line_size();
            let mut start = address.as_usize();
            let end = start + AlignedVirtualAddress::<SHIFT>::ALIGNMENT;

            while start < end {
                asm!("dc cvac, {}", in(reg) start, options(nostack, preserves_flags));
                start += cache_line_size;
            }

            system::barrier::barrier();
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
