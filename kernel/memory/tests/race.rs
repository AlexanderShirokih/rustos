//! Гонки за физическими фреймами: аллокатор один на систему, и его инварианты
//! обязаны держаться при одновременных запросах с разных CPU.

#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used, clippy::unwrap_in_result)]

mod common;

use std::{
    collections::HashSet,
    sync::{Arc, Barrier, Mutex},
    thread,
};

use collections::MutexCell;
use common::make_range;
use memory::{
    FrameBitmap,
    frame::Frame,
    frame_allocator::{FrameAllocator, PhysicalFrameAllocator},
};

type Allocator = PhysicalFrameAllocator<MutexCell<FrameBitmap>>;

const CONTENDERS: usize = 4;
const ROUNDS: usize = 512;

const REGIONS: [(usize, usize); 3] = [(64, 96), (512, 64), (4096, 48)];

const BLOCK_FRAMES: usize = 4;

fn allocator() -> Allocator {
    // Регионов больше одного: путь выбора региона тоже общий для CPU.
    let ranges: Vec<_> = REGIONS
        .iter()
        .map(|(start, count)| make_range(*start, *count))
        .collect();
    PhysicalFrameAllocator::new(ranges.into_iter()).expect("regions are valid")
}

fn drain_capacity(allocator: &Allocator) -> usize {
    let mut drained = Vec::new();
    while let Some(frame) = allocator.allocate_frame() {
        drained.push(frame);
    }
    let capacity = drained.len();
    for frame in drained {
        allocator
            .deallocate_frame(frame)
            .expect("a drained frame returns to its region");
    }
    capacity
}

#[derive(Default)]
struct Owners {
    frames: Mutex<HashSet<usize>>,
}

impl Owners {
    fn take(&self, frame: Frame, count: usize) -> Result<(), usize> {
        // Номер, уже лежащий в наборе, занят другим владельцем: аллокатор
        // выдал один фрейм дважды.
        let mut frames = self.frames.lock().expect("owners lock");
        for number in frame.number()..frame.number() + count {
            if !frames.insert(number) {
                return Err(number);
            }
        }
        Ok(())
    }

    fn release(&self, frame: Frame, count: usize) {
        let mut frames = self.frames.lock().expect("owners lock");
        for number in frame.number()..frame.number() + count {
            frames.remove(&number);
        }
    }
}

fn contend(work: impl Fn(usize) -> Option<usize> + Send + Sync + 'static) -> Vec<usize> {
    let work = Arc::new(work);
    let barrier = Arc::new(Barrier::new(CONTENDERS));
    let mut handles = Vec::with_capacity(CONTENDERS);
    for index in 0..CONTENDERS {
        let work = Arc::clone(&work);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            let mut doubled = Vec::new();
            for _ in 0..ROUNDS {
                if let Some(number) = work(index) {
                    doubled.push(number);
                }
            }
            doubled
        }));
    }
    handles
        .into_iter()
        .flat_map(|handle| handle.join().expect("contender joins"))
        .collect()
}

#[test]
fn race_frames_are_never_handed_out_twice() {
    let allocator = allocator();
    let capacity = drain_capacity(&allocator);
    let allocator = Arc::new(allocator);
    let owners = Arc::new(Owners::default());

    let doubled = {
        let allocator = Arc::clone(&allocator);
        let owners = Arc::clone(&owners);
        contend(move |_| {
            let frame = allocator.allocate_frame()?;
            let doubled = owners.take(frame, 1).err();
            owners.release(frame, 1);
            allocator
                .deallocate_frame(frame)
                .expect("an allocated frame returns to its region");
            doubled
        })
    };

    assert!(
        doubled.is_empty(),
        "frames {doubled:?} were handed out to more than one contender at a time"
    );
    assert_eq!(
        drain_capacity(&allocator),
        capacity,
        "every frame returns to the allocator once the contenders are done"
    );
}

#[test]
fn race_block_allocations_never_overlap_single_frames() {
    let allocator = allocator();
    let capacity = drain_capacity(&allocator);
    let allocator = Arc::new(allocator);
    let owners = Arc::new(Owners::default());

    let doubled = {
        let allocator = Arc::clone(&allocator);
        let owners = Arc::clone(&owners);
        // Половина соперников берёт смежные блоки, половина - одиночные фреймы. Блочный
        // путь ищет свободный участок, одиночный идёт от подсказки, и общий у них только
        // битмап.
        contend(move |index| {
            let (frame, count) = if index % 2 == 0 {
                allocator.allocate_frames(BLOCK_FRAMES)?
            } else {
                (allocator.allocate_frame()?, 1)
            };
            let doubled = owners.take(frame, count).err();
            owners.release(frame, count);
            for offset in 0..count {
                allocator
                    .deallocate_frame(frame.add(offset))
                    .expect("an allocated frame returns to its region");
            }
            doubled
        })
    };

    assert!(
        doubled.is_empty(),
        "frames {doubled:?} belonged to a block and to a single-frame owner at once"
    );
    assert_eq!(
        drain_capacity(&allocator),
        capacity,
        "every frame returns to the allocator once the contenders are done"
    );
}
