use alloc::boxed::Box;
use core::alloc::Layout;
use core::fmt::{Display, Formatter};
use core::ptr::NonNull;
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

    pub fn from_non_null(ptr: NonNull<u8>, size: usize) -> Self {
        MmioAddress {
            address: ptr.as_ptr() as usize,
            size,
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

pub type CleanupCallback = dyn FnOnce(NonNull<u8>, Layout, PageAlignedVirtualAddress, usize);

pub struct MmioBound {
    ptr: NonNull<u8>,
    layout: Layout,
    virtual_address: PageAlignedVirtualAddress,
    size: usize,
    cleanup: Option<Box<CleanupCallback>>,
}

unsafe impl Sync for MmioBound {}

impl MmioBound {
    pub fn new(
        ptr: NonNull<u8>,
        layout: Layout,
        virtual_address: PageAlignedVirtualAddress,
        size: usize,
        cleanup: Box<CleanupCallback>,
    ) -> Self {
        Self {
            ptr,
            layout,
            virtual_address,
            size,
            cleanup: Some(cleanup),
        }
    }

    pub fn base(&self) -> MmioAddress {
        MmioAddress::from_non_null(self.ptr, self.size)
    }

    pub fn write<T>(&self, offset: usize, val: T) {
        unsafe {
            let ptr = self.ptr.add(offset).cast();
            ptr.write_volatile(val)
        }
    }

    pub fn write_reg<T>(&self, reg: Reg<T>, val: T) {
        self.write(reg.offset, val)
    }

    pub fn read<T>(&self, offset: usize) -> T {
        unsafe {
            let ptr = self.ptr.add(offset).cast();
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
            cleanup(self.ptr, self.layout, self.virtual_address, self.size)
        }
    }
}
