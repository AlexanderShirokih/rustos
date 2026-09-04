#![allow(
    clippy::expect_used,
    clippy::std_instead_of_core,
    clippy::unwrap_in_result
)]

mod common;

use std::{
    boxed::Box,
    num::{NonZeroU32, NonZeroUsize},
    sync::{Arc, Mutex},
    vec,
};

use memory::{
    AccessMask, MemoryRegion,
    frame::Frame,
    frame_allocator::{FrameAllocator, FrameError, ReserveFrameError},
};
use scheduler::{AddressSpace, Process, ProcessId, ProcessName};

use crate::common::MockAddressSpaceFactory;

struct WitnessAllocator {
    factory: &'static MockAddressSpaceFactory,
    released_at_deallocate: Mutex<Vec<usize>>,
}

impl WitnessAllocator {
    fn new(factory: &'static MockAddressSpaceFactory) -> Self {
        Self {
            factory,
            released_at_deallocate: Mutex::new(Vec::new()),
        }
    }

    fn released_at_deallocate(&self) -> Vec<usize> {
        self.released_at_deallocate
            .lock()
            .expect("witness lock")
            .clone()
    }
}

impl FrameAllocator for WitnessAllocator {
    fn reserve_frames_exact(
        &self,
        from_inclusive: Frame,
        _to_exclusive: Frame,
    ) -> Result<Frame, ReserveFrameError> {
        Ok(from_inclusive)
    }

    fn allocate_frame(&self) -> Option<Frame> {
        Some(Frame::new(2000 + self.released_at_deallocate().len()))
    }

    fn allocate_frames(&self, _max_count: usize) -> Option<(Frame, usize)> {
        None
    }

    fn deallocate_frame(&self, _frame: Frame) -> Result<(), FrameError> {
        self.released_at_deallocate
            .lock()
            .expect("witness lock")
            .push(self.factory.released());
        Ok(())
    }

    fn is_allocated(&self, _frame: Frame) -> bool {
        false
    }
}

fn pid(raw: u32) -> ProcessId {
    ProcessId::new(NonZeroU32::new(raw).expect("non-zero pid"))
}

#[test]
fn image_frames_return_after_the_address_space_is_torn_down() {
    let factory: &'static MockAddressSpaceFactory =
        Box::leak(Box::new(MockAddressSpaceFactory::new()));
    let fa: &'static WitnessAllocator = Box::leak(Box::new(WitnessAllocator::new(factory)));

    let address_space = AddressSpace::new_user(factory).expect("create user AS");
    let segment = Arc::new(
        MemoryRegion::create_virtual(fa, NonZeroUsize::new(2).expect("non-zero"), AccessMask::RW)
            .expect("virtual region"),
    );

    let mut process = Process::empty(
        pid(1),
        ProcessName::new("image-owner").expect("valid name"),
        address_space,
    );
    process.set_image_segments(vec![segment]);

    assert_eq!(factory.released(), 0);
    assert!(fa.released_at_deallocate().is_empty());

    drop(process);

    let observed = fa.released_at_deallocate();
    assert_eq!(observed.len(), 2, "both image frames come back");
    assert!(
        observed.iter().all(|released| *released == 1),
        "frames returned while the mapper was still alive: {observed:?}"
    );
}
