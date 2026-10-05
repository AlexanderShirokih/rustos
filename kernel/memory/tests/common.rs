//! Общие помощники интеграционных тестов `memory`.
//!
//! Каждый файл в `tests/` компилируется отдельным крейтом, и помощник,
//! нужный одному файлу, виден остальным как `dead_code` - отсюда
//! `#[allow(dead_code)]` на элементах ниже.

#![allow(clippy::expect_used, clippy::indexing_slicing)]

use core::num::NonZeroUsize;
use std::sync::Arc;

use memory::{
    aligned::Aligned, memory::MemoryAccessProvider, memory_range::MemoryRange,
    physical_address::PageAlignedAddress, virtual_address::AlignedVirtualAddress,
};

#[allow(dead_code)]
const TEST_FRAME_SIZE: usize = PageAlignedAddress::ALIGNMENT;

#[allow(dead_code)]
pub(crate) fn frame_to_address(frame: usize) -> PageAlignedAddress {
    PageAlignedAddress::from_usize(frame * TEST_FRAME_SIZE).expect("frame_to_address: not aligned")
}

#[allow(dead_code)]
pub(crate) fn nz(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).expect("nz: zero")
}

#[allow(dead_code)]
pub(crate) fn make_range(
    start_frame: usize,
    frame_count: usize,
) -> MemoryRange<PageAlignedAddress> {
    assert!(frame_count > 0, "frame_count must be > 0");
    let start = frame_to_address(start_frame);
    let end = frame_to_address(start_frame + frame_count);
    MemoryRange::new(start, end)
}

struct MockMemoryAccessProviderInner {
    frame_size: usize,
    len: usize,
    data: spin::Mutex<Vec<u8>>,
}

#[derive(Clone)]
#[allow(dead_code)]
pub(crate) struct MockMemoryAccessProvider {
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

impl core::fmt::Debug for MockMemoryAccessProvider {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MockMemoryAccessProvider")
            .field("inner", &self.inner)
            .finish()
    }
}

#[allow(dead_code)]
impl MockMemoryAccessProvider {
    pub(crate) fn new(frame_size: usize, total_frames: usize) -> Self {
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

    pub(crate) fn total_bytes(&self) -> usize {
        self.inner.len
    }

    pub(crate) fn total_frames(&self) -> usize {
        self.inner.len / self.inner.frame_size
    }

    pub(crate) fn snapshot(&self, offset: usize, len: usize) -> Vec<u8> {
        assert!(offset + len <= self.inner.len, "snapshot out of bounds");
        let data = self.inner.data.lock();
        data[offset..offset + len].to_vec()
    }
}

impl MemoryAccessProvider for MockMemoryAccessProvider {
    fn clean_cache<const SHIFT: u8>(&self, _address: AlignedVirtualAddress<SHIFT>) {}

    fn invalidate_cache(&self) {}
}
