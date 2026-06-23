use alloc::sync::Arc;

use kobject::{CancelTarget, ThreadObject};
use memory::{MemoryRegion, virtual_address::VirtualAddress};

use super::arch::{ArchContext, CpuId, ThreadStack};
use crate::{Priority, ProcessId, ThreadId};

/// Backing per-thread IPC-буфера: user-VA замапленной страницы и владение `MemoryRegion`.
///
/// `region` - одна из двух ссылок на регион; фрейм освобождается только когда дропнуты
/// обе (вторая - `MappingTag` в `UserVmAllocator`). Изымать через [`Thread::take_ipc_buffer`].
pub struct IpcBufferSlot {
    user_va: VirtualAddress,
    #[allow(dead_code)]
    region: Arc<MemoryRegion>,
}

impl IpcBufferSlot {
    pub fn new(user_va: VirtualAddress, region: Arc<MemoryRegion>) -> Self {
        Self { user_va, region }
    }

    /// User-VA, по которому замаплен IPC-буфер текущего потока.
    pub fn user_va(&self) -> VirtualAddress {
        self.user_va
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadState {
    Ready,
    Running,
    Blocked,
    Sleeping { wakeup_at_ns: u64 },
    Terminated,
}

pub struct Thread<A: ArchContext> {
    id: ThreadId,
    process: ProcessId,
    cpu_affinity: CpuId,
    priority: Priority,
    state: ThreadState,
    time_slice_left: u32,
    arch: A,
    stack: ThreadStack,
    name: &'static str,
    ko: Arc<ThreadObject>,
    /// Per-thread IPC-буфер. `None` у kernel-потоков (нет user-памяти);
    /// у user-потоков заполняется при `prepare_user_thread`.
    ipc_buffer: Option<IpcBufferSlot>,
    /// Cancel-хук активной блокировки на Port: при терминации потока его
    /// `Waiter` снимается с очереди Port. `None`, когда поток не запаркован на
    /// Port (выставляется на время парковки, снимается после resolve).
    blocked_cancel: Option<Arc<dyn CancelTarget>>,
}

impl<A: ArchContext> Thread<A> {
    pub fn new(
        id: ThreadId,
        process: ProcessId,
        cpu_affinity: CpuId,
        priority: Priority,
        arch: A,
        stack: ThreadStack,
        name: &'static str,
    ) -> Self {
        Self {
            id,
            process,
            cpu_affinity,
            priority,
            state: ThreadState::Ready,
            time_slice_left: 0,
            arch,
            stack,
            name,
            ko: ThreadObject::new(),
            ipc_buffer: None,
            blocked_cancel: None,
        }
    }

    pub fn id(&self) -> ThreadId {
        self.id
    }

    pub fn process(&self) -> ProcessId {
        self.process
    }

    pub fn cpu_affinity(&self) -> CpuId {
        self.cpu_affinity
    }

    pub fn priority(&self) -> Priority {
        self.priority
    }

    pub fn state(&self) -> ThreadState {
        self.state
    }

    pub fn set_state(&mut self, state: ThreadState) {
        self.state = state;
    }

    pub fn time_slice_left(&self) -> u32 {
        self.time_slice_left
    }

    pub fn set_time_slice_left(&mut self, time_slice_left: u32) {
        self.time_slice_left = time_slice_left;
    }

    pub fn arch(&self) -> &A {
        &self.arch
    }

    pub fn arch_mut(&mut self) -> &mut A {
        &mut self.arch
    }

    pub fn stack(&self) -> &ThreadStack {
        &self.stack
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Lifecycle-KO потока; переживает удаление из `ThreadTable`.
    pub fn thread_object(&self) -> &Arc<ThreadObject> {
        &self.ko
    }

    /// User-VA per-thread IPC-буфера; `None` у kernel-потоков.
    pub fn ipc_buffer_va(&self) -> Option<VirtualAddress> {
        self.ipc_buffer.as_ref().map(IpcBufferSlot::user_va)
    }

    /// Прикрепляет backing IPC-буфера к потоку; вызывается один раз при создании user-потока.
    pub fn set_ipc_buffer(&mut self, slot: IpcBufferSlot) {
        self.ipc_buffer = Some(slot);
    }

    /// Изымает backing IPC-буфера при терминации.
    pub fn take_ipc_buffer(&mut self) -> Option<IpcBufferSlot> {
        self.ipc_buffer.take()
    }

    /// Привязывает cancel-хук блокировки на Port (на время парковки).
    pub fn set_blocked_cancel(&mut self, cancel: Arc<dyn CancelTarget>) {
        self.blocked_cancel = Some(cancel);
    }

    /// Изымает cancel-хук блокировки (после resolve либо при терминации).
    pub fn take_blocked_cancel(&mut self) -> Option<Arc<dyn CancelTarget>> {
        self.blocked_cancel.take()
    }
}
