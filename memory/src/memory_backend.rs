use crate::physical::{Frame, PageAlignedAddress, PhysicalAddress};
use core::marker::PhantomData;

/// Обертка указателя на физическую память
pub struct MemoryPtr<T> {
    addr: PageAlignedAddress,
    _phantom: PhantomData<T>,
}

impl<T> MemoryPtr<T> {
    pub fn new(address: PageAlignedAddress) -> Self {
        Self {
            addr: address,
            _phantom: PhantomData,
        }
    }

    #[inline(always)]
    pub fn addr(&self) -> PageAlignedAddress {
        self.addr
    }

    #[inline(always)]
    pub fn write<B: MemoryBackend>(&self, backend: &B, value: T)
    where
        T: Copy,
    {
        backend.write::<T>(self.addr.as_physical_address(), value);
    }
}

impl<T> From<PageAlignedAddress> for MemoryPtr<T> {
    #[inline(always)]
    fn from(addr: PageAlignedAddress) -> Self {
        MemoryPtr::new(addr)
    }
}

impl<T> From<Frame> for MemoryPtr<T> {
    #[inline(always)]
    fn from(frame: Frame) -> Self {
        MemoryPtr::new(frame.page_address())
    }
}

pub trait MemoryBackend {
    fn frame_size(&self) -> usize;

    fn read<T>(&self, addr: PhysicalAddress) -> T;
    fn write<T: Copy>(&self, addr: PhysicalAddress, val: T);

    fn clean_page_cache(&self, address: PhysicalAddress);
    fn invalidate_cache(&self);
}
