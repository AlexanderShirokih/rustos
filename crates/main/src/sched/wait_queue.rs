use alloc::collections::{BinaryHeap, VecDeque};
use core::cmp::Ordering;

use drivers_common::services::scheduler::ThreadId;

#[derive(Default)]
pub struct WaitQueue {
    entries: VecDeque<ThreadId>,
}

impl WaitQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, id: ThreadId) {
        self.entries.push_back(id);
    }

    pub fn pop(&mut self) -> Option<ThreadId> {
        self.entries.pop_front()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SleepEntry {
    pub wakeup_at_ns: u64,
    pub thread_id: ThreadId,
}

impl Ord for SleepEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .wakeup_at_ns
            .cmp(&self.wakeup_at_ns)
            .then_with(|| other.thread_id.cmp(&self.thread_id))
    }
}

impl PartialOrd for SleepEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Default)]
pub struct SleepQueue {
    entries: BinaryHeap<SleepEntry>,
}

impl SleepQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, entry: SleepEntry) {
        self.entries.push(entry);
    }

    pub fn peek(&self) -> Option<&SleepEntry> {
        self.entries.peek()
    }

    pub fn pop(&mut self) -> Option<SleepEntry> {
        self.entries.pop()
    }
}
