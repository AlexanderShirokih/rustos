use drivers_common::services::scheduler::ThreadId;

use super::{arch::CpuId, ready_queue::ReadyQueue};

/// Per-CPU состояние scheduler-а.
///
/// Хранится по стабильному адресу (`Box<Cpu<PRIO>>`), указатель на который
/// ARCH-слой записывает в CPU-local регистр (например, `TPIDR_EL1`).
pub struct Cpu<const PRIO: usize> {
    id: CpuId,
    ready_queue: ReadyQueue<PRIO>,
    current: ThreadId,
    idle: ThreadId,
}

impl<const PRIO: usize> Cpu<PRIO> {
    pub fn new(id: CpuId, idle: ThreadId) -> Self {
        Self {
            id,
            ready_queue: ReadyQueue::new(),
            current: idle,
            idle,
        }
    }

    pub fn id(&self) -> CpuId {
        self.id
    }

    pub fn ready_queue(&self) -> &ReadyQueue<PRIO> {
        &self.ready_queue
    }

    pub fn ready_queue_mut(&mut self) -> &mut ReadyQueue<PRIO> {
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
