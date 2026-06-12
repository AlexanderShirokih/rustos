//! `PhysicalResource` KO: capability-токен на минтинг `Memory`-регионов
//! поверх фиксированного физического диапазона (MMIO, DMA-буферы).
//!
//! Объект хранит границы и потолок доступа: право [`Rights::MINT`](super::Rights::MINT)
//! разрешает минтить только поддиапазоны этого ресурса, а сужение прав
//! пересылаемой копии делается через `handle_duplicate`.

use alloc::sync::Arc;
use core::num::NonZeroUsize;

use memory::{AccessMask, physical_address::PageAlignedAddress};

#[derive(Debug)]
pub struct PhysicalResource {
    pa_base: PageAlignedAddress,
    size_bytes: NonZeroUsize,
    access_mask: AccessMask,
}

impl PhysicalResource {
    pub fn new(
        pa_base: PageAlignedAddress,
        size_bytes: NonZeroUsize,
        access_mask: AccessMask,
    ) -> Arc<Self> {
        Arc::new(Self {
            pa_base,
            size_bytes,
            access_mask,
        })
    }

    pub fn pa_base(&self) -> PageAlignedAddress {
        self.pa_base
    }

    pub fn size_bytes(&self) -> NonZeroUsize {
        self.size_bytes
    }

    pub fn access_mask(&self) -> AccessMask {
        self.access_mask
    }

    pub fn permits(
        &self,
        pa_base: PageAlignedAddress,
        size_bytes: NonZeroUsize,
        access_mask: AccessMask,
    ) -> bool {
        if !self.access_mask.allows(access_mask) {
            return false;
        }

        let resource_start = self.pa_base.as_usize();
        let Some(resource_end) = resource_start.checked_add(self.size_bytes.get()) else {
            return false;
        };
        let requested_start = pa_base.as_usize();
        let Some(requested_end) = requested_start.checked_add(size_bytes.get()) else {
            return false;
        };

        resource_start <= requested_start && requested_end <= resource_end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pa(raw: usize) -> PageAlignedAddress {
        PageAlignedAddress::from_usize(raw).unwrap()
    }

    fn nz(raw: usize) -> NonZeroUsize {
        NonZeroUsize::new(raw).unwrap()
    }

    #[test]
    fn permits_subrange_with_subset_access() {
        let resource = PhysicalResource::new(pa(0x4000_0000), nz(0x4000), AccessMask::RW);

        assert!(resource.permits(pa(0x4000_1000), nz(0x1000), AccessMask::R));
    }

    #[test]
    fn rejects_range_outside_resource() {
        let resource = PhysicalResource::new(pa(0x4000_0000), nz(0x4000), AccessMask::RW);

        assert!(!resource.permits(pa(0x4000_3000), nz(0x2000), AccessMask::R));
        assert!(!resource.permits(pa(0x3fff_f000), nz(0x2000), AccessMask::R));
    }

    #[test]
    fn rejects_access_outside_resource_mask() {
        let resource = PhysicalResource::new(pa(0x4000_0000), nz(0x4000), AccessMask::R);

        assert!(!resource.permits(pa(0x4000_0000), nz(0x1000), AccessMask::RW));
    }

    #[test]
    fn rejects_overflowing_requested_range() {
        let resource = PhysicalResource::new(pa(0xffff_f000), nz(0x1000), AccessMask::R);

        assert!(!resource.permits(pa(0xffff_f000), nz(usize::MAX), AccessMask::R));
    }
}
