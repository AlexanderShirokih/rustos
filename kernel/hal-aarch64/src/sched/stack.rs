use alloc::vec;

use scheduler::{StackError, ThreadStack, ThreadStackAllocator};

const PAGE_SIZE: usize = 4096;

/// Allocator стека потока.
///
/// Защита от переполнения стека обеспечивается canary в начале стека
/// (`STACK_CANARY`), который scheduler проверяет при каждом context-switch.
pub struct Aarch64Stack;

impl ThreadStackAllocator for Aarch64Stack {
    fn allocate(pages: usize) -> Result<ThreadStack, StackError> {
        if pages == 0 {
            return Err(StackError::InvalidSize);
        }

        let size = pages
            .checked_mul(PAGE_SIZE)
            .ok_or(StackError::OutOfMemory)?;
        let bytes = vec![0u8; size].into_boxed_slice();
        ThreadStack::from_boxed_bytes(bytes)
    }
}
