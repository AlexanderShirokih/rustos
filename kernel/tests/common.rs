#![allow(dead_code)]

use std::{
    num::NonZeroU32,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    vec,
};

use drivers_common::services::scheduler::Priority;
use kernel::sched::{
    ArchContext, ArchCpu, ArchStack, CpuId, ProcessId, StackError, Thread, ThreadStack,
    TimerSource,
};

static SWITCH_COUNT: AtomicUsize = AtomicUsize::new(0);

#[derive(Default)]
pub struct MockContext;

pub struct MockCpu;

pub struct MockStack;

pub struct MockTimer {
    now_ns: AtomicU64,
    scheduled_ns: AtomicU64,
}

#[derive(Clone)]
pub struct MockTimerSource(pub Arc<MockTimer>);

impl MockTimer {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            now_ns: AtomicU64::new(0),
            scheduled_ns: AtomicU64::new(0),
        })
    }

    pub fn advance_to(&self, now_ns: u64) {
        self.now_ns.store(now_ns, Ordering::SeqCst);
    }

    pub fn scheduled_deadline(&self) -> u64 {
        self.scheduled_ns.load(Ordering::SeqCst)
    }
}

impl TimerSource for MockTimerSource {
    fn now_ns(&self) -> u64 {
        self.0.now_ns.load(Ordering::SeqCst)
    }

    fn schedule_next(&self, deadline_ns: u64) {
        self.0.scheduled_ns.store(deadline_ns, Ordering::SeqCst);
    }
}

impl ArchContext for MockContext {
    type Cpu = MockCpu;
    type Stack = MockStack;

    fn init(
        _stack_top: core::ptr::NonNull<u8>,
        _entry: kernel::sched::arch::TrampolineFn,
        _arg: *mut (),
    ) -> Self {
        Self
    }

    unsafe fn start(_next: &Self) -> ! {
        panic!("MockContext::start is not used in host tests")
    }

    unsafe fn switch(_prev: &mut Self, _next: &Self) {
        SWITCH_COUNT.fetch_add(1, Ordering::SeqCst);
    }
}

impl ArchCpu for MockCpu {
    fn current_id() -> CpuId {
        CpuId::new(0)
    }

    fn idle() -> ! {
        panic!("idle thread must not execute in host tests")
    }
}

impl ArchStack for MockStack {
    fn allocate(pages: usize) -> Result<ThreadStack, StackError> {
        if pages == 0 {
            return Err(StackError::InvalidSize);
        }

        let bytes = vec![0u8; pages * 4096].into_boxed_slice();
        ThreadStack::from_boxed_bytes(bytes)
    }
}

pub fn reset_switches() {
    SWITCH_COUNT.store(0, Ordering::SeqCst);
}

pub fn switch_count() -> usize {
    SWITCH_COUNT.load(Ordering::SeqCst)
}

pub fn make_thread(name: &'static str, priority: Priority) -> Thread<MockContext> {
    let process = ProcessId::new(NonZeroU32::new(1).expect("non-zero"));
    let stack = MockStack::allocate(1).expect("mock stack allocation");
    let arch = MockContext::default();
    Thread::new(process, priority, arch, stack, name)
}
