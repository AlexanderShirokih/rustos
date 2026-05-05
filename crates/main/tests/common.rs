#![allow(dead_code)]
#![allow(unsafe_code)]

use std::{cell::Cell, num::NonZeroU32, sync::Arc, vec};

use drivers_common::services::scheduler::{Priority, ThreadId};
use main::sched::{
    ArchContext, ArchCpu, CpuId, ProcessId, StackError, Thread, ThreadStack, ThreadStackAllocator,
    TimerSource,
};

thread_local! {
    static SWITCH_COUNT: Cell<usize> = const { Cell::new(0) };
    static IRQ_DEPTH: Cell<usize> = const { Cell::new(0) };
    static MAX_IRQ_DEPTH: Cell<usize> = const { Cell::new(0) };
    static CPU_LOCAL_PTR: Cell<usize> = const { Cell::new(0) };
}

#[derive(Default)]
pub struct MockContext;

pub struct MockCpu;

pub struct MockStack;

pub struct MockTimer {
    now_ns: std::sync::atomic::AtomicU64,
    scheduled_ns: std::sync::atomic::AtomicU64,
}

#[derive(Clone)]
pub struct MockTimerSource(pub Arc<MockTimer>);

impl MockTimer {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            now_ns: std::sync::atomic::AtomicU64::new(0),
            scheduled_ns: std::sync::atomic::AtomicU64::new(0),
        })
    }

    pub fn advance_to(&self, now_ns: u64) {
        self.now_ns
            .store(now_ns, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn scheduled_deadline(&self) -> u64 {
        self.scheduled_ns.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl TimerSource for MockTimerSource {
    fn now_ns(&self) -> u64 {
        self.0.now_ns.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn schedule_next(&self, deadline_ns: u64) {
        self.0
            .scheduled_ns
            .store(deadline_ns, std::sync::atomic::Ordering::SeqCst);
    }
}

impl ArchContext for MockContext {
    type Cpu = MockCpu;
    type Stack = MockStack;

    fn init(
        _stack_top: core::ptr::NonNull<u8>,
        _entry: main::sched::arch::TrampolineFn,
        _arg: *mut (),
    ) -> Self {
        Self
    }

    unsafe fn start(_next: &Self) -> ! {
        panic!("MockContext::start is not used in host tests")
    }

    unsafe fn switch(_prev: &mut Self, _next: &Self) {
        SWITCH_COUNT.with(|c| c.set(c.get() + 1));
    }
}

impl ArchCpu for MockCpu {
    fn current_id() -> CpuId {
        CpuId::new(0)
    }

    unsafe fn install_cpu_local(cpu: *mut ()) {
        CPU_LOCAL_PTR.with(|c| c.set(cpu as usize));
    }

    fn cpu_local_ptr() -> *mut () {
        CPU_LOCAL_PTR.with(|c| c.get()) as *mut ()
    }

    fn idle() -> ! {
        panic!("idle thread must not execute in host tests")
    }

    fn enable_preemption() {
        IRQ_DEPTH.with(|c| {
            let depth = c.get();
            assert!(depth > 0, "enable_preemption without prior disable");
            c.set(depth - 1);
        });
    }

    fn disable_preemption() {
        IRQ_DEPTH.with(|c| {
            let depth = c.get() + 1;
            c.set(depth);
            MAX_IRQ_DEPTH.with(|m| {
                if m.get() < depth {
                    m.set(depth);
                }
            });
        });
    }
}

impl ThreadStackAllocator for MockStack {
    fn allocate(pages: usize) -> Result<ThreadStack, StackError> {
        if pages == 0 {
            return Err(StackError::InvalidSize);
        }

        let bytes = vec![0u8; pages * 4096].into_boxed_slice();
        ThreadStack::from_boxed_bytes(bytes)
    }
}

pub fn reset_switches() {
    SWITCH_COUNT.with(|c| c.set(0));
    IRQ_DEPTH.with(|c| c.set(0));
    MAX_IRQ_DEPTH.with(|c| c.set(0));
    CPU_LOCAL_PTR.with(|c| c.set(0));
}

pub fn switch_count() -> usize {
    SWITCH_COUNT.with(|c| c.get())
}

pub fn current_irq_depth() -> usize {
    IRQ_DEPTH.with(|c| c.get())
}

pub fn max_irq_depth() -> usize {
    MAX_IRQ_DEPTH.with(|c| c.get())
}

pub fn make_thread(name: &'static str, priority: Priority) -> Thread<MockContext> {
    let process = ProcessId::new(NonZeroU32::new(1).expect("non-zero"));
    let stack = MockStack::allocate(1).expect("mock stack allocation");
    let arch = MockContext;
    let id = ThreadId::new(NonZeroU32::new(1).expect("non-zero"));
    Thread::new(id, process, CpuId::new(0), priority, arch, stack, name)
}
