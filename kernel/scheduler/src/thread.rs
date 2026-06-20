use alloc::sync::Arc;

use kobject::ThreadObject;
use memory::{MemoryRegion, virtual_address::VirtualAddress};

use super::arch::{ArchContext, CpuId, ThreadStack};
use crate::{Priority, ProcessId, ThreadId};

/// Backing per-thread IPC-buffer'а (как в seL4): user-VA замапленной страницы
/// и владение `MemoryRegion`, чтобы фрейм жил ровно столько же, сколько поток.
///
/// `region` держит сильную ссылку: при удалении `Thread` из `ThreadTable`
/// `Arc` дропается, и - если ссылок больше нет - фреймы возвращаются в
/// `FrameAllocator` через `Drop` региона. PTE снимаются вместе с
/// AddressSpace процесса.
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
}
