use alloc::vec;

use memory::PAGE_SIZE;
use scheduler::{StackError, ThreadStack, ThreadStackAllocator};

pub(crate) struct Aarch64Stack;

impl ThreadStackAllocator for Aarch64Stack {
    fn allocate(pages: usize) -> Result<ThreadStack, StackError> {
        if pages == 0 {
            return Err(StackError::InvalidSize);
        }

        let size = pages
            .checked_mul(PAGE_SIZE.get())
            .ok_or(StackError::OutOfMemory)?;
        let bytes = vec![0u8; size].into_boxed_slice();
        ThreadStack::from_boxed_bytes(bytes)
    }
}
