#![allow(dead_code)]
#![allow(unsafe_code)]

use std::{
    cell::{Cell, RefCell},
    num::NonZeroU32,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    vec,
    vec::Vec,
};

use drivers_common::services::scheduler::{Priority, ThreadId};
use main::sched::{
    ArchContext, ArchCpu, CpuId, ProcessId, StackError, Thread, ThreadStack, ThreadStackAllocator,
    TimerSource,
};
use memory::{
    MemFlags,
    memory_mapper::{
        AddressSpaceFactory, AsCreateError, MemoryMapper, MemoryMappingError, MemoryRemappingError,
        MemoryUnmappingError,
    },
    physical_address::{PageAlignedAddress, PhysicalAddress},
    virtual_address::PageAlignedVirtualAddress,
};

thread_local! {
    static SWITCH_COUNT: Cell<usize> = const { Cell::new(0) };
    static IRQ_DEPTH: Cell<usize> = const { Cell::new(0) };
    static MAX_IRQ_DEPTH: Cell<usize> = const { Cell::new(0) };
    static CPU_LOCAL_PTR: Cell<usize> = const { Cell::new(0) };
    static ADDRESS_SPACE_SWITCHES: RefCell<Vec<Option<usize>>> = const { RefCell::new(Vec::new()) };
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

    fn switch_address_space(next_root: Option<PhysicalAddress>) {
        let raw = next_root.map(|pa| pa.as_usize());
        ADDRESS_SPACE_SWITCHES.with(|c| c.borrow_mut().push(raw));
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
    ADDRESS_SPACE_SWITCHES.with(|c| c.borrow_mut().clear());
}

/// Извлекает накопленную последовательность вызовов
/// `MockContext::switch_address_space` и очищает буфер.
pub fn take_address_space_switches() -> Vec<Option<usize>> {
    ADDRESS_SPACE_SWITCHES.with(|c| std::mem::take(&mut *c.borrow_mut()))
}

/// Последний root, переданный в `MockContext::switch_address_space`.
pub fn last_address_space_root() -> Option<Option<usize>> {
    ADDRESS_SPACE_SWITCHES.with(|c| c.borrow().last().copied())
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

/// Mock-mapper: stub-реализация, нужен только для уникального `root_pa`.
/// Все mapping-операции возвращают `Unsupported`/`OutOfMemory` -
/// host-тесты scheduler-а их не используют.
struct MockUserMapper {
    root_pa: PhysicalAddress,
    /// Счётчик `released`, увеличиваемый в Drop. `Arc<AtomicUsize>` шарится
    /// с фабрикой.
    released: Arc<AtomicUsize>,
}

impl MemoryMapper for MockUserMapper {
    fn map(
        &self,
        _start_address: &PageAlignedVirtualAddress,
        _size: usize,
    ) -> Result<(), MemoryMappingError> {
        Err(MemoryMappingError::OutOfMemory)
    }

    fn map_exact(
        &self,
        _source_address: PageAlignedVirtualAddress,
        _target_address: PageAlignedAddress,
        _size: usize,
        _mem_flags: MemFlags,
    ) -> Result<(), MemoryMappingError> {
        Err(MemoryMappingError::OutOfMemory)
    }

    fn unmap(
        &self,
        _address: PageAlignedVirtualAddress,
        _size: usize,
    ) -> Result<(), MemoryUnmappingError> {
        Err(MemoryUnmappingError::Unsupported)
    }

    fn remap(
        &self,
        _start_address: PageAlignedVirtualAddress,
        _size: usize,
        _new_flags: MemFlags,
    ) -> Result<(), MemoryRemappingError> {
        Err(MemoryRemappingError::NotMapped)
    }

    fn root_pa(&self) -> PhysicalAddress {
        self.root_pa
    }

    #[cfg(feature = "qemu-tests")]
    fn query_leaf_raw(&self, _address: PageAlignedVirtualAddress) -> Option<u64> {
        None
    }
}

impl Drop for MockUserMapper {
    fn drop(&mut self) {
        self.released.fetch_add(1, Ordering::SeqCst);
    }
}

/// Mock-фабрика user-AS: каждая `create_user`-вызов возвращает уникальный
/// `MockUserMapper` со свежим `root_pa = base + index * PAGE_SIZE`.
pub struct MockAddressSpaceFactory {
    inner: Mutex<MockFactoryInner>,
    released: Arc<AtomicUsize>,
}

struct MockFactoryInner {
    next_root_pa: usize,
    created: usize,
}

impl MockAddressSpaceFactory {
    /// Базовый PA выбран в lower-half, заведомо не пересекается с типичными
    /// тестовыми регионами.
    pub const BASE_ROOT_PA: usize = 0x10_0000;

    pub fn new() -> Self {
        Self {
            inner: Mutex::new(MockFactoryInner {
                next_root_pa: Self::BASE_ROOT_PA,
                created: 0,
            }),
            released: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn created(&self) -> usize {
        self.inner.lock().unwrap().created
    }

    pub fn released(&self) -> usize {
        self.released.load(Ordering::SeqCst)
    }
}

impl Default for MockAddressSpaceFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl AddressSpaceFactory for MockAddressSpaceFactory {
    fn create_user(&self) -> Result<Box<dyn MemoryMapper + Send + Sync>, AsCreateError> {
        let mut inner = self.inner.lock().unwrap();
        let root_pa = PhysicalAddress::new(inner.next_root_pa);
        inner.next_root_pa += 4096;
        inner.created += 1;
        Ok(Box::new(MockUserMapper {
            root_pa,
            released: self.released.clone(),
        }))
    }
}
