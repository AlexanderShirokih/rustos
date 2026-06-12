//! Глобальные слоты ядерных сервисов для мест без `KernelContext`-а на руках.

use alloc::sync::Arc;

use memory::{frame_allocator::FrameAllocator, memory_mapper::AddressSpaceFactory};
use scheduler::SchedulerService;
use spin::Once;

static SCHEDULER: Once<Arc<dyn SchedulerService>> = Once::new();
static ADDRESS_SPACE_FACTORY: Once<&'static (dyn AddressSpaceFactory + Send + Sync)> = Once::new();
static FRAME_ALLOCATOR: Once<&'static (dyn FrameAllocator + Send + Sync)> = Once::new();

pub fn install_scheduler(service: Arc<dyn SchedulerService>) {
    assert!(
        SCHEDULER.get().is_none(),
        "SchedulerService is already installed in syscall bridge"
    );
    let _ = SCHEDULER.call_once(|| service);
}

pub fn scheduler() -> &'static Arc<dyn SchedulerService> {
    SCHEDULER
        .get()
        .expect("SchedulerService must be installed via syscall_bridge::install_scheduler")
}

pub fn install_address_space_factory(factory: &'static (dyn AddressSpaceFactory + Send + Sync)) {
    assert!(
        ADDRESS_SPACE_FACTORY.get().is_none(),
        "AddressSpaceFactory is already installed in syscall bridge"
    );
    let _ = ADDRESS_SPACE_FACTORY.call_once(|| factory);
}

pub fn address_space_factory() -> Option<&'static (dyn AddressSpaceFactory + Send + Sync)> {
    ADDRESS_SPACE_FACTORY.get().copied()
}

pub fn install_frame_allocator(allocator: &'static (dyn FrameAllocator + Send + Sync)) {
    assert!(
        FRAME_ALLOCATOR.get().is_none(),
        "FrameAllocator is already installed in syscall bridge"
    );
    let _ = FRAME_ALLOCATOR.call_once(|| allocator);
}

pub fn frame_allocator() -> Option<&'static (dyn FrameAllocator + Send + Sync)> {
    FRAME_ALLOCATOR.get().copied()
}
