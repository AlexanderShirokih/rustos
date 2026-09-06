#![allow(unsafe_code)]

use alloc::{boxed::Box, string::String};
use core::fmt::{Display, Formatter};

use io::mmio::{Reg, VolatileInt};
use memory::{
    mem_flags::{Owners, UncachedPermission},
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
// он используется только из `Drop` (эксклюзивный `&mut self`), поэтому гонки исключены.
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

    /// `true`, если `len` байт от смещения `offset` лежат внутри области.
    pub fn contains(&self, offset: usize, len: usize) -> bool {
        offset
            .checked_add(len)
            .is_some_and(|end| end <= self.mmio_address.size())
    }

    fn in_bounds<T>(&self, offset: usize) -> bool {
        offset.is_multiple_of(align_of::<T>()) && self.contains(offset, size_of::<T>())
    }

    /// Доступ мимо области отбрасывается: `MmioBound` владеет ровно `size`
    /// байтами от базы. Проверка молчалива - на `MmioBound` завязана запись в
    /// консоль, и диагностика отсюда замыкает её на себя. Смещение, снятое
    /// с геометрии device tree, драйвер проверяет сам через
    /// [`MmioBound::contains`]. Смещение, ограниченное номером регистра,
    /// лежит внутри области по построению.
    pub fn write<T: VolatileInt>(&self, offset: usize, val: T) {
        if !self.in_bounds::<T>(offset) {
            return;
        }

        let ptr: *mut T = self.virtual_address.as_ptr::<T>().wrapping_byte_add(offset);
        // SAFETY: `MmioBound` владеет замапленным `[virtual_address; mmio_address.size]`-регионом
        // (инвариант `MmioService::map_mmio`); проверка выше оставила
        // `offset + size_of::<T>()` внутри региона и выровненным,
        // `write_volatile` корректен для MMIO.
        unsafe { ptr.write_volatile(val) };
    }

    pub fn write_reg<T: VolatileInt>(&self, reg: Reg<T>, val: T) {
        self.write(reg.offset, val);
    }

    /// Чтение мимо области возвращает ноль. О самой границе - [`MmioBound::write`].
    pub fn read<T: VolatileInt>(&self, offset: usize) -> T {
        if !self.in_bounds::<T>(offset) {
            return T::default();
        }

        let ptr: *const T = self.virtual_address.as_ptr::<T>().wrapping_byte_add(offset);
        // SAFETY: см. `write` - регион замаплен и принадлежит этому `MmioBound`,
        // проверка выше оставила регистр внутри региона, `read_volatile` корректен для MMIO.
        unsafe { ptr.read_volatile() }
    }

    pub fn read_reg<T: VolatileInt>(&self, reg: Reg<T>) -> T {
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
        permissions: Owners<UncachedPermission>,
    ) -> Result<MmioBound, MmioMapError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bound(size: usize) -> MmioBound {
        MmioBound::new(
            MmioAddress::new(0x1000, size).expect("non-zero base"),
            PageAlignedVirtualAddress::from_usize(0x4000).expect("aligned"),
            Box::new(|_, _| {}),
        )
    }

    #[test]
    fn range_up_to_the_upper_edge_is_contained() {
        assert!(bound(64).contains(0, 64));
        assert!(bound(64).contains(32, 32));
    }

    #[test]
    fn range_crossing_the_upper_edge_is_not_contained() {
        assert!(!bound(64).contains(32, 33));
        assert!(!bound(64).contains(64, 1));
    }

    #[test]
    fn range_overflowing_usize_is_not_contained() {
        assert!(!bound(64).contains(usize::MAX, 1));
    }

    #[test]
    fn empty_range_inside_the_region_is_contained() {
        assert!(bound(64).contains(64, 0));
        assert!(!bound(64).contains(65, 0));
    }

    #[test]
    fn register_at_the_last_aligned_offset_is_in_bounds() {
        assert!(bound(8).in_bounds::<u32>(4));
        assert!(bound(8).in_bounds::<u64>(0));
    }

    #[test]
    fn register_crossing_the_upper_edge_is_out_of_bounds() {
        assert!(!bound(8).in_bounds::<u32>(8));
        assert!(!bound(6).in_bounds::<u32>(4));
    }

    #[test]
    fn offset_overflowing_usize_is_out_of_bounds() {
        assert!(!bound(8).in_bounds::<u32>(usize::MAX - 1));
    }

    #[test]
    fn misaligned_offset_is_out_of_bounds() {
        assert!(!bound(64).in_bounds::<u32>(2));
        assert!(!bound(64).in_bounds::<u64>(4));
    }
}
