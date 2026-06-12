use super::{arch::CpuId, ready_queue::ReadyQueue};
use crate::ThreadId;

/// Per-CPU состояние планировщика.
pub struct Cpu {
    id: CpuId,
    ready_queue: ReadyQueue,
    current: ThreadId,
    idle: ThreadId,
}

impl Cpu {
    pub fn new(id: CpuId, idle: ThreadId, priority_levels: usize) -> Self {
        Self {
            id,
            ready_queue: ReadyQueue::new(priority_levels),
            current: idle,
            idle,
        }
    }

    pub fn id(&self) -> CpuId {
        self.id
    }

    pub fn ready_queue(&self) -> &ReadyQueue {
        &self.ready_queue
    }

    pub fn ready_queue_mut(&mut self) -> &mut ReadyQueue {
        &mut self.ready_queue
    }

    pub fn current(&self) -> ThreadId {
        self.current
    }

    pub fn set_current(&mut self, current: ThreadId) {
        self.current = current;
    }

    pub fn idle(&self) -> ThreadId {
        self.idle
    }
}
