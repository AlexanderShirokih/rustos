#![allow(dead_code)]
#![allow(unsafe_code)]
#![allow(clippy::unwrap_used)]
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::std_instead_of_core,
    clippy::unwrap_in_result
)]

use std::{
    cell::{Cell, RefCell},
    num::NonZeroU32,
    sync::Arc,
    vec,
    vec::Vec,
};

use memory::memory_mapper::AddressSpaceHandle;
use scheduler::{
    ArchContext, ArchCpu, CpuId, CpuLocalError, Priority, ProcessId, StackError, Thread, ThreadId,
    ThreadStack, ThreadStackAllocator, TimerSource,
};

const PAGE_SIZE: usize = memory::PAGE_SIZE.get();

thread_local! {
    static SWITCH_COUNT: Cell<usize> = const { Cell::new(0) };
    static PREEMPTION_ENABLED: Cell<bool> = const { Cell::new(true) };
    static CPU_LOCAL_PTR: Cell<usize> = const { Cell::new(0) };
    static ADDRESS_SPACE_SWITCHES: RefCell<Vec<Option<AddressSpaceHandle>>> =
        const { RefCell::new(Vec::new()) };
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

    const USER_VA_END: usize = 0x0001_0000_0000_0000;

    fn init(
        _stack_top: core::ptr::NonNull<u8>,
        _entry: scheduler::arch::TrampolineFn,
        _arg: *mut (),
    ) -> Self {
        Self
    }

    unsafe fn start(_next: &Self) -> ! {
        panic!("MockContext::start is not used in host tests")
    }

    unsafe fn switch(_prev: &mut Self, _next: &Self) {
        assert!(
            !preemption_enabled(),
            "context switch must run with preemption disabled"
        );
        SWITCH_COUNT.with(|c| c.set(c.get() + 1));
    }

    fn switch_address_space(next: Option<AddressSpaceHandle>) {
        ADDRESS_SPACE_SWITCHES.with(|c| c.borrow_mut().push(next));
    }

    fn init_user(_entry: scheduler::UserEntry) -> Self {
        Self
    }
}

impl ArchCpu for MockCpu {
    fn current_id() -> CpuId {
        CpuId::ZERO
    }

    unsafe fn install_cpu_local(cpu: *mut ()) -> Result<(), CpuLocalError> {
        CPU_LOCAL_PTR.with(|c| c.set(cpu as usize));
        Ok(())
    }

    fn cpu_local_ptr() -> *mut () {
        CPU_LOCAL_PTR.with(Cell::get) as *mut ()
    }

    fn idle() -> ! {
        panic!("idle thread must not execute in host tests")
    }

    fn enable_preemption() {
        PREEMPTION_ENABLED.with(|c| c.set(true));
    }

    fn disable_preemption() {
        PREEMPTION_ENABLED.with(|c| c.set(false));
    }

    fn preemption_enabled() -> bool {
        PREEMPTION_ENABLED.with(Cell::get)
    }
}

impl ThreadStackAllocator for MockStack {
    fn allocate(pages: usize) -> Result<ThreadStack, StackError> {
        if pages == 0 {
            return Err(StackError::InvalidSize);
        }

        let bytes = vec![0u8; pages * PAGE_SIZE].into_boxed_slice();
        ThreadStack::from_boxed_bytes(bytes)
    }
}

pub fn reset_switches() {
    SWITCH_COUNT.with(|c| c.set(0));
    PREEMPTION_ENABLED.with(|c| c.set(true));
    CPU_LOCAL_PTR.with(|c| c.set(0));
    ADDRESS_SPACE_SWITCHES.with(|c| c.borrow_mut().clear());
}

pub fn take_address_space_switches() -> Vec<Option<AddressSpaceHandle>> {
    ADDRESS_SPACE_SWITCHES.with(|c| std::mem::take(&mut *c.borrow_mut()))
}

/// Последний handle, переданный в `MockContext::switch_address_space`.
/// Внешний `Option` - был ли вообще вызов. Внутренний - значение `next`,
/// которое могло быть `None` для перехода на kernel-thread.
#[allow(clippy::option_option)]
pub fn last_address_space_root() -> Option<Option<AddressSpaceHandle>> {
    ADDRESS_SPACE_SWITCHES.with(|c| c.borrow().last().copied())
}

pub fn switch_count() -> usize {
    SWITCH_COUNT.with(Cell::get)
}

pub fn preemption_enabled() -> bool {
    PREEMPTION_ENABLED.with(Cell::get)
}

pub fn make_thread(name: &'static str, priority: Priority) -> Thread<MockContext> {
    let process = ProcessId::new(NonZeroU32::new(1).expect("non-zero"));
    let stack = MockStack::allocate(1).expect("mock stack allocation");
    let arch = MockContext;
    let id = ThreadId::new(NonZeroU32::new(1).expect("non-zero"));
    Thread::new(id, process, CpuId::ZERO, priority, arch, stack, name)
}
