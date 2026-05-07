//! AArch64-фабрика per-process user-AS (TTBR0_EL1 lower-half).
//!
//! Выделяет L0-фрейм через `&'static FrameAllocator`, инициализирует пустую
//! `PageTable<L0>` и оборачивает в `Aarch64MemoryMapper` с MutexCell стратегией
//! и higher-half offset (тем же, что и kernel-mapper) - благодаря этому
//! последующие `map_exact` смогут аллоцировать промежуточные таблицы и
//! обращаться к ним через линейное higher-half-отображение.

use alloc::boxed::Box;

use collections::{MutexCell, NoLockCell};
use hal_aarch64_paging::{
    level::L0, mapper::PageMapper, mem_flags::Aarch64MemFlags, page_table::PageTable,
};
use memory::{
    FrameBitmap,
    frame_allocator::{FrameAllocator, PhysicalFrameAllocator},
    memory_mapper::{AddressSpaceFactory, AsCreateError, MemoryMapper},
    virtual_address::PageAlignedVirtualAddress,
};

use crate::memory::memory_mapper::{Aarch64MemoryMapper, FrameTableAlloc};

type FrameAllocatorImpl = PhysicalFrameAllocator<NoLockCell<FrameBitmap>>;
type MutexPageMapper<'a, FA> = MutexCell<PageMapper<FrameTableAlloc<'a, FA>>>;

/// Фабрика user-AS, привязанная к глобальному `FrameAllocator` и
/// higher-half offset для трансляции PA таблиц в VA.
pub struct Aarch64AddressSpaceFactory {
    frame_allocator: &'static FrameAllocatorImpl,
    higher_half_base: PageAlignedVirtualAddress,
}

// SAFETY: `frame_allocator` - `&'static`-shared single-CPU аллокатор
// с внутренней синхронизацией через `&self`-методы; никаких изменяемых
// полей в фабрике нет.
unsafe impl Send for Aarch64AddressSpaceFactory {}
// SAFETY: см. `Send`.
unsafe impl Sync for Aarch64AddressSpaceFactory {}

impl Aarch64AddressSpaceFactory {
    pub fn new(
        frame_allocator: &'static FrameAllocatorImpl,
        higher_half_base: PageAlignedVirtualAddress,
    ) -> Self {
        Self {
            frame_allocator,
            higher_half_base,
        }
    }
}

impl AddressSpaceFactory for Aarch64AddressSpaceFactory {
    fn create_user(&self) -> Result<Box<dyn MemoryMapper + Send + Sync>, AsCreateError> {
        let frame = self
            .frame_allocator
            .allocate_frame()
            .ok_or(AsCreateError::OutOfMemory)?;
        let root_pa = frame.page_address();
        let offset = self.higher_half_base.as_usize();

        // Линейное higher-half отображение PA->VA: VA = PA + offset.
        let root_ptr = (root_pa.as_usize() + offset) as *mut PageTable<L0>;
        // SAFETY: свежевыделенный фрейм, эксклюзивно принадлежит фабрике;
        // запись пустой PageTable инициализирует все entries в Invalid.
        unsafe { root_ptr.write(PageTable::<L0>::new()) };

        let mapper: Aarch64MemoryMapper<'static, _, MutexPageMapper<'static, _>> =
            Aarch64MemoryMapper::new_with_offset(
                self.frame_allocator,
                root_ptr,
                Aarch64MemFlags::new(),
                offset,
            );

        Ok(Box::new(mapper))
    }
}
