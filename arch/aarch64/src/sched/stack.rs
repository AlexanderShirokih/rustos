use alloc::vec;

use kernel::sched::{ArchStack, StackError, ThreadStack};

pub struct Aarch64Stack;

impl ArchStack for Aarch64Stack {
    fn allocate(pages: usize) -> Result<ThreadStack, StackError> {
        if pages == 0 {
            return Err(StackError::InvalidSize);
        }

        let size = pages.checked_mul(4096).ok_or(StackError::OutOfMemory)?;
        let bytes = vec![0u8; size].into_boxed_slice();
        ThreadStack::from_boxed_bytes(bytes)
    }
}
