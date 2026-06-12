use alloc::{collections::VecDeque, vec, vec::Vec};

use crate::{Priority, ThreadId};

const fn validate_priority_levels(priority_levels: usize) {
    assert!(priority_levels > 0 && priority_levels <= 32);
}

/// Очередь runnable-потоков: per-уровневые FIFO + bitmap для O(1) выбора старшего приоритета.
pub struct ReadyQueue {
    priority_levels: usize,
    bitmap: u32,
    queues: Vec<VecDeque<ThreadId>>,
}

impl ReadyQueue {
    pub fn new(priority_levels: usize) -> Self {
        validate_priority_levels(priority_levels);

        Self {
            bitmap: 0,
            priority_levels,
            queues: vec![VecDeque::new(); priority_levels],
        }
    }

    pub fn push(&mut self, id: ThreadId, priority: Priority) {
        let level = self.priority_index(priority);
        self.queues[level].push_back(id);
        self.bitmap |= Self::mask_for(level);
    }

    pub fn pop_highest(&mut self) -> Option<(ThreadId, Priority)> {
        let level = self.highest_level()?;
        let id = self.queues[level].pop_front()?;
        if self.queues[level].is_empty() {
            self.bitmap &= !Self::mask_for(level);
        }
        Some((id, Priority::new(level as u8)))
    }

    pub fn peek_highest_priority(&self) -> Option<Priority> {
        let level = self.highest_level()?;
        Some(Priority::new(level as u8))
    }

    pub fn is_empty(&self) -> bool {
        self.bitmap == 0
    }

    fn highest_level(&self) -> Option<usize> {
        if self.bitmap == 0 {
            return None;
        }

        let level = self.bitmap.leading_zeros() as usize;
        if level < self.priority_levels {
            Some(level)
        } else {
            None
        }
    }

    fn priority_index(&self, priority: Priority) -> usize {
        let level = priority.raw() as usize;
        assert!(
            level < self.priority_levels,
            "priority level {level} exceeds queue size {}",
            self.priority_levels
        );
        level
    }

    fn mask_for(level: usize) -> u32 {
        1u32 << (31 - level)
    }
}

impl Default for ReadyQueue {
    fn default() -> Self {
        Self::new(32)
    }
}
