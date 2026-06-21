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

#[cfg(test)]
mod tests {
    extern crate std;

    use std::{
        boxed::Box,
        panic::{AssertUnwindSafe, catch_unwind},
    };

    use memory::{
        frame::Frame,
        frame_allocator::{FrameError, ReserveFrameError},
        memory_mapper::{AddressSpaceFactory, AsCreateError, MemoryMapper},
    };
    use scheduler::{SpawnConfig, SpawnError, ThreadId};

    use super::*;

    struct DummyScheduler;

    impl SchedulerService for DummyScheduler {
        fn spawn_boxed(
            &self,
            _cfg: SpawnConfig,
            _entry: Box<dyn FnOnce() + Send + 'static>,
        ) -> Result<ThreadId, SpawnError> {
            unimplemented!()
        }
        fn yield_now(&self) {}
        fn sleep_ns(&self, _ns: u64) {}
        fn current(&self) -> ThreadId {
            ThreadId::new(core::num::NonZeroU32::new(1).unwrap())
        }
        fn exit(&self) -> ! {
            panic!("exit")
        }
    }

    struct DummyFactory;

    impl AddressSpaceFactory for DummyFactory {
        fn create_user(&self) -> Result<Arc<dyn MemoryMapper + Send + Sync>, AsCreateError> {
            Err(AsCreateError::OutOfMemory)
        }
    }

    struct DummyFrameAllocator;

    impl FrameAllocator for DummyFrameAllocator {
        fn reserve_frames_exact(
            &self,
            _from: Frame,
            _to: Frame,
        ) -> Result<Frame, ReserveFrameError> {
            unimplemented!()
        }
        fn allocate_frame(&self) -> Option<Frame> {
            None
        }
        fn allocate_frames(&self, _max: usize) -> Option<(Frame, usize)> {
            None
        }
        fn deallocate_frame(&self, _frame: Frame) -> Result<(), FrameError> {
            Ok(())
        }
        fn is_allocated(&self, _frame: Frame) -> bool {
            false
        }
    }

    #[test]
    fn scheduler_install_getter_and_double_install_panic() {
        assert!(catch_unwind(scheduler).is_err());

        let service: Arc<dyn SchedulerService> = Arc::new(DummyScheduler);
        install_scheduler(service);

        assert_eq!(scheduler().current().raw().get(), 1);

        let again: Arc<dyn SchedulerService> = Arc::new(DummyScheduler);
        assert!(catch_unwind(AssertUnwindSafe(|| install_scheduler(again))).is_err());
    }

    #[test]
    fn address_space_factory_install_getter_and_double_install_panic() {
        assert!(address_space_factory().is_none());

        let factory: &'static DummyFactory = Box::leak(Box::new(DummyFactory));
        install_address_space_factory(factory);

        assert!(address_space_factory().is_some());

        let again: &'static DummyFactory = Box::leak(Box::new(DummyFactory));
        assert!(catch_unwind(AssertUnwindSafe(|| install_address_space_factory(again))).is_err());
    }

    #[test]
    fn frame_allocator_install_getter_and_double_install_panic() {
        assert!(frame_allocator().is_none());

        let fa: &'static DummyFrameAllocator = Box::leak(Box::new(DummyFrameAllocator));
        install_frame_allocator(fa);

        assert!(frame_allocator().is_some());

        let again: &'static DummyFrameAllocator = Box::leak(Box::new(DummyFrameAllocator));
        assert!(catch_unwind(AssertUnwindSafe(|| install_frame_allocator(again))).is_err());
    }
}
