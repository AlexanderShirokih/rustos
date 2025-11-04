use crate::physical::{Frame, PhysicalAddress};
use core::marker::PhantomData;
use core::mem::MaybeUninit;

/// Обертка указателя на физическую память
pub struct MemoryPtr<T> {
    addr: PhysicalAddress,
    _phantom: PhantomData<T>,
}

impl<T> MemoryPtr<T> {
    #[inline(always)]
    pub unsafe fn new(address: PhysicalAddress) -> Self {
        assert_eq!(address.0 % align_of::<T>(), 0, "Misaligned pointer");

        Self {
            addr: address,
            _phantom: PhantomData,
        }
    }

    #[inline(always)]
    pub fn addr(&self) -> PhysicalAddress {
        self.addr
    }

    #[inline(always)]
    pub fn write<B: MemoryBackendExt>(&self, backend: &B, value: T)
    where
        T: Copy,
    {
        backend.write::<T>(self.addr, value);
    }
}

impl<T> From<PhysicalAddress> for MemoryPtr<T> {
    #[inline(always)]
    fn from(addr: PhysicalAddress) -> Self {
        unsafe { MemoryPtr::new(addr) }
    }
}

impl<T> From<(Frame, usize)> for MemoryPtr<T> {
    #[inline(always)]
    fn from((frame, frame_size): (Frame, usize)) -> Self {
        let addr = frame.start_address(frame_size);
        unsafe { MemoryPtr::new(addr) }
    }
}

pub trait MemoryBackend {
    fn frame_size(&self) -> usize;
    fn read_bytes(&self, addr: PhysicalAddress, buf: &mut [u8]);
    fn write_bytes(&self, addr: PhysicalAddress, buf: &[u8]);
    fn enable_virtual_mode(&self, root_page: PhysicalAddress);
    fn clean_page_cache(&self, address: PhysicalAddress);
    fn invalidate_cache(&self);
}

pub trait MemoryBackendExt: MemoryBackend {
    #[inline(always)]
    fn read<T>(&self, addr: PhysicalAddress) -> T {
        let mut val = MaybeUninit::<T>::uninit();
        unsafe {
            let buf = core::slice::from_raw_parts_mut(val.as_mut_ptr() as *mut u8, size_of::<T>());
            self.read_bytes(addr, buf);
            val.assume_init()
        }
    }

    #[inline(always)]
    fn write<T: Copy>(&self, addr: PhysicalAddress, val: T) {
        unsafe {
            let buf = core::slice::from_raw_parts(&val as *const T as *const u8, size_of::<T>());
            self.write_bytes(addr, buf);
        }
    }
}

impl<T: MemoryBackend> MemoryBackendExt for T {}
