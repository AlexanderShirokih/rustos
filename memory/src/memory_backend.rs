use crate::physical::{Frame, PageAlignedAddress, PhysicalAddress};
use alloc::sync::Arc;
use alloc::{vec, vec::Vec};
use core::marker::PhantomData;
use core::mem::{MaybeUninit, size_of};
use core::ops::Range;
use spin::Mutex;

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

    fn enable_virtual_mode(&self, root_page: PhysicalAddress);
    fn clean_page_cache(&self, address: PhysicalAddress);
    fn invalidate_cache(&self);
}

/// Внутреннее состояние MockMemoryBackend
struct MockMemoryBackendInner {
    frame_size: usize,
    len: usize,
    data: Mutex<Vec<u8>>,
    last_root_page: Mutex<Option<PhysicalAddress>>,
}

/// Простая реализация MemoryBackend для модульных тестов.
///
/// Хранит содержимое памяти в векторе и позволяет инспектировать его из тестов.
/// Clone создаёт ещё одну ссылку на те же данные.
#[derive(Debug, Clone)]
pub struct MockMemoryBackend {
    inner: Arc<MockMemoryBackendInner>,
}

impl core::fmt::Debug for MockMemoryBackendInner {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MockMemoryBackendInner")
            .field("frame_size", &self.frame_size)
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

impl MockMemoryBackend {
    /// Создаёт backend с заданным размером фрейма и количеством фреймов.
    pub fn new(frame_size: usize, total_frames: usize) -> Self {
        let len = frame_size
            .checked_mul(total_frames)
            .expect("frame_size * total_frames overflow");

        Self {
            inner: Arc::new(MockMemoryBackendInner {
                frame_size,
                len,
                data: Mutex::new(vec![0u8; len]),
                last_root_page: Mutex::new(None),
            }),
        }
    }

    /// Полный объём памяти в байтах.
    pub fn total_bytes(&self) -> usize {
        self.inner.len
    }

    /// Количество фреймов.
    pub fn total_frames(&self) -> usize {
        self.inner.len / self.inner.frame_size
    }

    /// Возвращает адрес корневой таблицы, переданный при enable_virtual_mode().
    pub fn last_root_page(&self) -> Option<PhysicalAddress> {
        *self.inner.last_root_page.lock()
    }

    /// Снимок диапазона памяти (для проверок в тестах).
    pub fn snapshot(&self, offset: usize, len: usize) -> Vec<u8> {
        assert!(offset + len <= self.inner.len, "snapshot out of bounds");
        let data = self.inner.data.lock();
        data[offset..offset + len].to_vec()
    }

    fn checked_range(&self, addr: PhysicalAddress, len: usize) -> Range<usize> {
        let start = addr.as_usize();
        let end = start
            .checked_add(len)
            .expect("address + len overflow in MockMemoryBackend");
        assert!(end <= self.inner.len, "memory access out of bounds");
        start..end
    }

    fn read_bytes(&self, addr: PhysicalAddress, buf: &mut [u8]) {
        let range = self.checked_range(addr, buf.len());
        let data = self.inner.data.lock();
        buf.copy_from_slice(&data[range]);
    }

    fn write_bytes(&self, addr: PhysicalAddress, buf: &[u8]) {
        let range = self.checked_range(addr, buf.len());
        let mut data = self.inner.data.lock();
        data[range].copy_from_slice(buf);
    }
}

impl MemoryBackend for MockMemoryBackend {
    fn frame_size(&self) -> usize {
        self.inner.frame_size
    }

    fn read<T>(&self, addr: PhysicalAddress) -> T {
        let mut val = MaybeUninit::<T>::uninit();
        unsafe {
            let buf = core::slice::from_raw_parts_mut(val.as_mut_ptr() as *mut u8, size_of::<T>());
            self.read_bytes(addr, buf);
            val.assume_init()
        }
    }

    fn write<T: Copy>(&self, addr: PhysicalAddress, val: T) {
        unsafe {
            let buf = core::slice::from_raw_parts(&val as *const T as *const u8, size_of::<T>());
            self.write_bytes(addr, buf);
        }
    }

    fn enable_virtual_mode(&self, root_page: PhysicalAddress) {
        *self.inner.last_root_page.lock() = Some(root_page);
    }

    fn clean_page_cache(&self, _address: PhysicalAddress) {}

    fn invalidate_cache(&self) {}
}
