use alloc::boxed::Box;
use core::fmt::{Display, Formatter};
use io::mmio::Reg;
use memory::virtual_address::PageAlignedVirtualAddress;

/// Адрес MMIO-региона.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MmioAddress {
    address: usize,
    size: usize,
}

impl MmioAddress {
    pub const fn new(address: usize, size: usize) -> Option<Self> {
        if address == 0usize {
            None
        } else {
            Some(MmioAddress { address, size })
        }
    }

    pub const fn base(&self) -> usize {
        self.address
    }

    pub const fn size(&self) -> usize {
        self.size
    }
}

impl Display for MmioAddress {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "0x{:x}(size 0x{:x})", self.address, self.size)
    }
}

pub type CleanupCallback = dyn FnOnce(PageAlignedVirtualAddress, usize);

pub struct MmioBound {
    mmio_address: MmioAddress,
    virtual_address: PageAlignedVirtualAddress,
    cleanup: Option<Box<CleanupCallback>>,
}

unsafe impl Sync for MmioBound {}

impl MmioBound {
    pub fn new(
        mmio_address: MmioAddress,
        virtual_address: PageAlignedVirtualAddress,
        cleanup: Box<CleanupCallback>,
    ) -> Self {
        Self {
            mmio_address,
            virtual_address,
            cleanup: Some(cleanup),
        }
    }

    pub fn base(&self) -> MmioAddress {
        self.mmio_address
    }

    pub fn write<T>(&self, offset: usize, val: T) {
        unsafe {
            let ptr: *mut T = self.virtual_address.as_ptr::<T>().byte_add(offset);
            ptr.write_volatile(val)
        }
    }

    pub fn write_reg<T>(&self, reg: Reg<T>, val: T) {
        self.write(reg.offset, val)
    }

    pub fn read<T>(&self, offset: usize) -> T {
        unsafe {
            let ptr = self.virtual_address.as_ptr::<T>().byte_add(offset);
            ptr.read_volatile()
        }
    }

    pub fn read_reg<T>(&self, reg: Reg<T>) -> T {
        self.read(reg.offset)
    }
}

impl Drop for MmioBound {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            cleanup(self.virtual_address, self.mmio_address.size)
        }
    }
}
