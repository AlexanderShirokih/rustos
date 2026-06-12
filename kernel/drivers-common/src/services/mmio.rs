#![allow(unsafe_code)]

use alloc::{boxed::Box, string::String};
use core::fmt::{Display, Formatter};

use io::mmio::Reg;
use memory::{
    mem_flags::{DeviceMemoryPermission, Owners},
    virtual_address::PageAlignedVirtualAddress,
};

// Адрес MMIO-региона.
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

// SAFETY: MMIO-доступы выполняются через `read_volatile`/`write_volatile` и потокобезопасны
// на уровне устройства; единственное не-`Sync`-поле - `Box<dyn FnOnce>` cleanup-колбэк,
// он используется только из `Drop` (эксклюзивный `&mut self`), поэтому гонок нет.
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
        // SAFETY: `MmioBound` владеет замапленным `[virtual_address; mmio_address.size]`-регионом
        // (инвариант `MmioService::map_mmio`); caller передаёт `offset < size` и тип `T`
        // соответствующий регистру, `write_volatile` корректен для MMIO.
        unsafe {
            let ptr: *mut T = self.virtual_address.as_ptr::<T>().byte_add(offset);
            ptr.write_volatile(val);
        }
    }

    pub fn write_reg<T>(&self, reg: Reg<T>, val: T) {
        self.write(reg.offset, val);
    }

    pub fn read<T>(&self, offset: usize) -> T {
        // SAFETY: см. `write` - регион замаплен и принадлежит этому `MmioBound`,
        // тип `T` соответствует регистру, `read_volatile` корректен для MMIO.
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
            cleanup(self.virtual_address, self.mmio_address.size);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmioMapError(pub String);

impl Display for MmioMapError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "MmioMapError: {}", self.0)
    }
}

pub trait MmioService: Send + Sync {
    fn map_mmio(
        &self,
        address: MmioAddress,
        permissions: Owners<DeviceMemoryPermission>,
    ) -> Result<MmioBound, MmioMapError>;
}
