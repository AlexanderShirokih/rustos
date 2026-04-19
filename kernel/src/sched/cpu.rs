use core::marker::PhantomData;

use drivers_common::services::scheduler::ThreadId;

use super::{arch::{ArchContext, CpuId}, ready_queue::ReadyQueue};

pub struct Cpu<A: ArchContext, const PRIO: usize> {
    id: CpuId,
    ready_queue: ReadyQueue<PRIO>,
    current: ThreadId,
    idle: ThreadId,
    _marker: PhantomData<A>,
}

impl<A: ArchContext, const PRIO: usize> Cpu<A, PRIO> {
    pub fn new(id: CpuId, idle: ThreadId) -> Self {
        Self {
            id,
            ready_queue: ReadyQueue::new(),
            current: idle,
            idle,
            _marker: PhantomData,
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
