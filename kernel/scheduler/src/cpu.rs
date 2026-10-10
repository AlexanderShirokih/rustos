use super::{arch::CpuId, ready_queue::ReadyQueue};
use crate::ThreadId;

/// Per-CPU состояние планировщика.
pub struct Cpu {
    id: CpuId,
    ready_queue: ReadyQueue,
    current: ThreadId,
    previous: Option<ThreadId>,
    idle: ThreadId,
}

impl Cpu {
    pub fn new(id: CpuId, idle: ThreadId, priority_levels: usize) -> Self {
        Self {
            id,
            ready_queue: ReadyQueue::new(priority_levels),
            current: idle,
            previous: None,
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

    /// Поток, чей контекст этот CPU сохраняет прямо сейчас.
    ///
    /// Взведён от выдачи `ScheduleAction::Switch` до возврата из переключения.
    /// Всё это время указатель на контекст остаётся вне scheduler-лока.
    pub fn previous(&self) -> Option<ThreadId> {
        self.previous
    }

    pub fn set_previous(&mut self, previous: Option<ThreadId>) {
        self.previous = previous;
    }

    pub fn idle(&self) -> ThreadId {
        self.idle
    }

    /// CPU исполняет свой idle-поток.
    pub fn is_idle(&self) -> bool {
        self.current == self.idle
    }
}
