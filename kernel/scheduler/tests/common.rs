#![allow(dead_code)]
#![allow(unsafe_code)]
#![allow(clippy::unwrap_used)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::std_instead_of_core,
    clippy::unwrap_in_result
)]

use std::{
    cell::{Cell, RefCell},
    num::{NonZeroU32, NonZeroUsize},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    vec,
    vec::Vec,
};

use memory::{
    frame::Frame,
    frame_allocator::{FrameAllocator, FrameError, ReserveFrameError},
    memory_mapper::AddressSpaceHandle,
};
use scheduler::{
    ArchContext, ArchCpu, CpuId, CpuLocalError, Priority, ProcessId, StackError, Thread, ThreadId,
    ThreadStack, ThreadStackAllocator, TimerSource,
};

const PAGE_SIZE: usize = memory::PAGE_SIZE.get();

thread_local! {
    static SWITCH_COUNT: Cell<usize> = const { Cell::new(0) };
    static PREEMPTION_ENABLED: Cell<bool> = const { Cell::new(true) };
    static CURRENT_CPU: Cell<usize> = const { Cell::new(0) };
    static CPU_LOCAL_PTRS: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
    static RESCHEDULES: RefCell<Vec<CpuId>> = const { RefCell::new(Vec::new()) };
    static ADDRESS_SPACE_SWITCHES: RefCell<Vec<Option<AddressSpaceHandle>>> =
        const { RefCell::new(Vec::new()) };
}

#[derive(Default)]
pub(crate) struct MockContext;

pub(crate) struct MockCpu;

pub(crate) struct MockStack;

pub(crate) struct MockTimer {
    now_ns: std::sync::atomic::AtomicU64,
    scheduled_ns: std::sync::atomic::AtomicU64,
}

#[derive(Clone)]
pub(crate) struct MockTimerSource(pub Arc<MockTimer>);

impl MockTimer {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            now_ns: std::sync::atomic::AtomicU64::new(0),
            scheduled_ns: std::sync::atomic::AtomicU64::new(0),
        })
    }

    pub(crate) fn advance_to(&self, now_ns: u64) {
        self.now_ns
            .store(now_ns, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn scheduled_deadline(&self) -> u64 {
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
        CpuId::new(CURRENT_CPU.with(Cell::get) as u16).unwrap_or(CpuId::ZERO)
    }

    unsafe fn install_cpu_local(cpu: *mut ()) -> Result<(), CpuLocalError> {
        let index = CURRENT_CPU.with(Cell::get);
        CPU_LOCAL_PTRS.with(|slots| {
            let mut slots = slots.borrow_mut();
            if slots.len() <= index {
                slots.resize(index + 1, 0);
            }
            slots[index] = cpu as usize;
        });
        Ok(())
    }

    fn cpu_local_ptr() -> *mut () {
        let index = CURRENT_CPU.with(Cell::get);
        CPU_LOCAL_PTRS.with(|slots| slots.borrow().get(index).copied().unwrap_or(0)) as *mut ()
    }

    fn send_reschedule(target: CpuId) {
        RESCHEDULES.with(|c| c.borrow_mut().push(target));
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

pub(crate) fn reset_switches() {
    SWITCH_COUNT.with(|c| c.set(0));
    PREEMPTION_ENABLED.with(|c| c.set(true));
    CURRENT_CPU.with(|c| c.set(0));
    CPU_LOCAL_PTRS.with(|c| c.borrow_mut().clear());
    RESCHEDULES.with(|c| c.borrow_mut().clear());
    ADDRESS_SPACE_SWITCHES.with(|c| c.borrow_mut().clear());
}

pub(crate) fn set_current_cpu(id: CpuId) {
    CURRENT_CPU.with(|c| c.set(id.as_index()));
}

pub(crate) fn take_reschedules() -> Vec<CpuId> {
    RESCHEDULES.with(|c| std::mem::take(&mut *c.borrow_mut()))
}

pub(crate) fn take_address_space_switches() -> Vec<Option<AddressSpaceHandle>> {
    ADDRESS_SPACE_SWITCHES.with(|c| std::mem::take(&mut *c.borrow_mut()))
}

#[allow(clippy::option_option)]
pub(crate) fn last_address_space_root() -> Option<Option<AddressSpaceHandle>> {
    ADDRESS_SPACE_SWITCHES.with(|c| c.borrow().last().copied())
}

pub(crate) fn switch_count() -> usize {
    SWITCH_COUNT.with(Cell::get)
}

pub(crate) fn preemption_enabled() -> bool {
    PREEMPTION_ENABLED.with(Cell::get)
}

pub(crate) fn with_simulated_irq<R>(f: impl FnOnce() -> R) -> R {
    let was_enabled = MockCpu::preemption_enabled();
    MockCpu::disable_preemption();
    let result = f();
    if was_enabled {
        MockCpu::enable_preemption();
    }
    result
}

pub(crate) fn make_thread(name: &'static str, priority: Priority) -> Thread<MockContext> {
    let process = ProcessId::new(NonZeroU32::new(1).expect("non-zero"));
    let stack = MockStack::allocate(1).expect("mock stack allocation");
    let arch = MockContext;
    let id = ThreadId::new(NonZeroU32::new(1).expect("non-zero"));
    Thread::new(id, process, CpuId::ZERO, priority, arch, stack, name)
}

pub(crate) struct CountingFrameAllocator {
    next: AtomicUsize,
    deallocated: Mutex<Vec<Frame>>,
}

impl CountingFrameAllocator {
    pub(crate) fn new() -> Self {
        Self {
            next: AtomicUsize::new(1000),
            deallocated: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn deallocated_count(&self) -> usize {
        self.deallocated.lock().unwrap().len()
    }
}

impl Default for CountingFrameAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameAllocator for CountingFrameAllocator {
    fn reserve_frames_exact(
        &self,
        from_inclusive: Frame,
        _to_exclusive: Frame,
    ) -> Result<Frame, ReserveFrameError> {
        Ok(from_inclusive)
    }

    fn allocate_frame(&self) -> Option<Frame> {
        Some(Frame::new(self.next.fetch_add(1, Ordering::SeqCst)))
    }

    fn allocate_frames(&self, _count: NonZeroUsize) -> Option<Frame> {
        None
    }

    fn deallocate_frame(&self, frame: Frame) -> Result<(), FrameError> {
        self.deallocated.lock().unwrap().push(frame);
        Ok(())
    }

    fn is_allocated(&self, _frame: Frame) -> bool {
        false
    }
}
