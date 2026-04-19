use alloc::vec;

use kernel::sched::{ArchStack, StackError, ThreadStack};

const PAGE_SIZE: usize = 4096;

/// Allocator стека потока.
///
/// TODO(stack-guard): полноценный MMU-based guard page (через
/// `MemoryMapper::unmap`) пока не поддерживается AArch64-маппером
/// (см. `arch/aarch64/src/memory/memory_mapper.rs::unmap`). До его
/// реализации защита переполнения стека обеспечивается canary в начале
/// стека (`STACK_CANARY`), который scheduler проверяет при каждом
/// context-switch.
pub struct Aarch64Stack;

impl ArchStack for Aarch64Stack {
    fn allocate(pages: usize) -> Result<ThreadStack, StackError> {
        if pages == 0 {
            return Err(StackError::InvalidSize);
        }

        let size = pages.checked_mul(PAGE_SIZE).ok_or(StackError::OutOfMemory)?;
        let bytes = vec![0u8; size].into_boxed_slice();
        ThreadStack::from_boxed_bytes(bytes)
    }
}
