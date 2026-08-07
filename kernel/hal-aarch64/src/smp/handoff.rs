//! Параметры, которые boot CPU передаёт стартующему.
//!
//! Стартующий CPU входит с выключенными MMU и кэшами: он читает эту память
//! мимо кэша и по физическому адресу. Boot CPU публикует записи, вытесняя их
//! до точки когерентности.

use core::sync::atomic::{AtomicU64, Ordering};

use hal_common::smp::OnlineCpus;
use scheduler::MAX_CPUS;

use crate::memory::regs::{common::EL1, dcache::DataCache};

pub(super) const SECONDARY_STACK_SIZE: usize = 16 * 1024;

#[repr(C, align(128))]
pub(super) struct SecondaryStack([u8; SECONDARY_STACK_SIZE]);

pub(super) const MAX_SECONDARIES: usize = MAX_CPUS - 1;

pub(super) static mut SECONDARY_STACKS: [SecondaryStack; MAX_SECONDARIES] =
    [const { SecondaryStack([0; SECONDARY_STACK_SIZE]) }; MAX_SECONDARIES];

#[derive(Clone, Copy)]
pub(super) struct SecondaryStackIndex(usize);

impl SecondaryStackIndex {
    pub(super) fn new(index: usize) -> Option<Self> {
        (index < MAX_SECONDARIES).then_some(Self(index))
    }

    pub(super) fn raw(self) -> usize {
        self.0
    }
}

#[repr(C, align(128))]
pub(super) struct Handoff {
    pending_id: AtomicU64,
    pending_stack: AtomicU64,
    lower_root: AtomicU64,
    higher_root: AtomicU64,
}

pub(super) static HANDOFF: Handoff = Handoff {
    pending_id: AtomicU64::new(0),
    pending_stack: AtomicU64::new(0),
    lower_root: AtomicU64::new(0),
    higher_root: AtomicU64::new(0),
};

pub(super) static ONLINE: OnlineCpus = OnlineCpus::new();

pub(super) struct TranslationRoots {
    pub lower: u64,
    pub higher: u64,
}

pub(super) fn publish_roots(lower_root: u64, higher_root: u64) {
    HANDOFF.lower_root.store(lower_root, Ordering::Relaxed);
    HANDOFF.higher_root.store(higher_root, Ordering::Relaxed);
    flush();
}

pub(super) fn publish_pending(id: u64, stack: SecondaryStackIndex) {
    HANDOFF.pending_id.store(id, Ordering::Relaxed);
    HANDOFF
        .pending_stack
        .store(stack.raw() as u64, Ordering::Relaxed);
    flush();
}

pub(super) fn roots() -> TranslationRoots {
    TranslationRoots {
        lower: HANDOFF.lower_root.load(Ordering::Relaxed),
        higher: HANDOFF.higher_root.load(Ordering::Relaxed),
    }
}

fn flush() {
    let start = core::ptr::from_ref(&HANDOFF) as usize;
    DataCache::<EL1>::clean_range_to_coherency(start, start + size_of::<Handoff>());
}
