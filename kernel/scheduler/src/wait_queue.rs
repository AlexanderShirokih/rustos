use alloc::collections::{BinaryHeap, VecDeque};
use core::cmp::Reverse;

use crate::ThreadId;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SleepEntry {
    pub wakeup_at_ns: u64,
    pub thread_id: ThreadId,
}

#[derive(Default)]
pub struct SleepQueue {
    entries: BinaryHeap<Reverse<SleepEntry>>,
}

impl SleepQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, entry: SleepEntry) {
        self.entries.push(Reverse(entry));
    }

    pub fn peek(&self) -> Option<&SleepEntry> {
        self.entries.peek().map(|Reverse(e)| e)
    }

    pub fn pop(&mut self) -> Option<SleepEntry> {
        self.entries.pop().map(|Reverse(e)| e)
    }
}
