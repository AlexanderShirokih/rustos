use alloc::{collections::VecDeque, vec, vec::Vec};

use crate::{Priority, ThreadId};

const fn validate_priority_levels(priority_levels: usize) {
    assert!(priority_levels > 0 && priority_levels <= 32);
}

/// Очередь runnable-потоков: per-уровневые FIFO + bitmap для O(1) выбора старшего приоритета.
pub struct ReadyQueue {
    priority_levels: usize,
    bitmap: u32,
    len: usize,
    queues: Vec<VecDeque<ThreadId>>,
}

impl ReadyQueue {
    pub fn new(priority_levels: usize) -> Self {
        validate_priority_levels(priority_levels);

        Self {
            bitmap: 0,
            len: 0,
            priority_levels,
            queues: vec![VecDeque::new(); priority_levels],
        }
    }

    pub fn push(&mut self, id: ThreadId, priority: Priority) {
        let level = self.priority_index(priority);
        self.queues[level].push_back(id);
        self.bitmap |= Self::mask_for(level);
        self.len += 1;
    }

    pub fn pop_highest(&mut self) -> Option<(ThreadId, Priority)> {
        let level = self.highest_level()?;
        let id = self.queues[level].pop_front()?;
        if self.queues[level].is_empty() {
            self.bitmap &= !Self::mask_for(level);
        }
        self.len = self.len.saturating_sub(1);
        Some((id, Priority::new(level as u8)))
    }

    /// Число потоков в очереди. Мера загрузки CPU при выборе, куда поставить
    /// новый поток.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn peek_highest_priority(&self) -> Option<Priority> {
        let level = self.highest_level()?;
        Some(Priority::new(level as u8))
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
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

#[cfg(test)]
mod tests {
    use core::num::NonZeroU32;

    use super::*;

    fn thread(raw: u32) -> ThreadId {
        ThreadId::new(NonZeroU32::new(raw).expect("nonzero id"))
    }

    #[test]
    fn length_counts_threads_across_priorities() {
        let mut queue = ReadyQueue::new(8);
        assert_eq!(queue.len(), 0);

        queue.push(thread(1), Priority::new(0));
        queue.push(thread(2), Priority::new(7));
        queue.push(thread(3), Priority::new(0));

        assert_eq!(queue.len(), 3, "длина - мера загрузки CPU при выборе");
    }

    #[test]
    fn length_falls_back_to_zero_when_drained() {
        let mut queue = ReadyQueue::new(4);
        queue.push(thread(1), Priority::new(1));
        queue.push(thread(2), Priority::new(3));

        assert!(queue.pop_highest().is_some());
        assert_eq!(queue.len(), 1);
        assert!(queue.pop_highest().is_some());

        assert_eq!(queue.len(), 0);
        assert!(queue.is_empty());
        assert!(queue.pop_highest().is_none());
        assert_eq!(queue.len(), 0, "пустая очередь не уходит в минус");
    }
}
