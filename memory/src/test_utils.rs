extern crate alloc;

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use crate::memory::MemoryAccessProvider;
use crate::virtual_address::AlignedVirtualAddress;

/// Внутренние данные мока для MemoryAccessProvider
struct MockMemoryAccessProviderInner {
    frame_size: usize,
    len: usize,
    data: spin::Mutex<Vec<u8>>,
}

/// Простая реализация MemoryAccessProvider для модульных тестов.
///
/// Хранит содержимое памяти в векторе и позволяет инспектировать его из тестов.
/// Clone создаёт ещё одну ссылку на те же данные.
#[derive(Debug, Clone)]
pub struct MockMemoryAccessProvider {
    inner: Arc<MockMemoryAccessProviderInner>,
}

impl core::fmt::Debug for MockMemoryAccessProviderInner {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MockMemoryAccessProviderInner")
            .field("frame_size", &self.frame_size)
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

impl MockMemoryAccessProvider {
    pub fn new(frame_size: usize, total_frames: usize) -> Self {
        let len = frame_size
            .checked_mul(total_frames)
            .expect("frame_size * total_frames overflow");

        Self {
            inner: Arc::new(MockMemoryAccessProviderInner {
                frame_size,
                len,
                data: spin::Mutex::new(vec![0u8; len]),
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

    /// Снимок диапазона памяти (для проверок в тестах).
    pub fn snapshot(&self, offset: usize, len: usize) -> Vec<u8> {
        assert!(offset + len <= self.inner.len, "snapshot out of bounds");
        let data = self.inner.data.lock();
        data[offset..offset + len].to_vec()
    }
}

impl MemoryAccessProvider for MockMemoryAccessProvider {
    fn clean_cache<const SHIFT: u8>(&self, _address: AlignedVirtualAddress<SHIFT>) {}

    fn invalidate_cache(&self) {}
}
