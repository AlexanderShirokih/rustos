use crate::aligned::Aligned;
use crate::physical_address::{PageAlignedAddress, PhysicalAddress};

/// Фрейм физической памяти.
///
/// Представляет страницу физической памяти фиксированного размера (4 КБ).
/// Хранит номер фрейма, а не адрес.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Frame(usize);

impl Frame {
    pub const fn new(number: usize) -> Self {
        Self(number)
    }

    #[must_use]
    pub const fn add(&self, frames_offset: usize) -> Self {
        Self::new(self.0 + frames_offset)
    }

    #[must_use]
    pub const fn sub(&self, frames_offset: usize) -> Self {
        Self::new(self.0 - frames_offset)
    }

    pub const fn page_address(&self) -> PageAlignedAddress {
        let address = self.0 * PageAlignedAddress::ALIGNMENT;
        PageAlignedAddress::new_unchecked(PhysicalAddress::new(address))
    }

    pub fn containing_address(address: PhysicalAddress) -> Self {
        Self(address.as_usize() / PageAlignedAddress::ALIGNMENT)
    }

    pub const fn number(&self) -> usize {
        self.0
    }
}

impl From<PageAlignedAddress> for Frame {
    fn from(addr: PageAlignedAddress) -> Self {
        Frame::new(addr.as_usize() / PageAlignedAddress::ALIGNMENT)
    }
}
