use alloc::{collections::VecDeque, vec, vec::Vec};

use crate::{Priority, ThreadId};

const fn validate_priority_levels(priority_levels: usize) {
    assert!(priority_levels > 0 && priority_levels <= 32);
}

/// Поток в очереди вместе с уровнем, на котором он стоит.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueuedThread {
    pub thread: ThreadId,
    pub priority: Priority,
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
        let Some(queue) = self.queues.get_mut(level) else {
            return;
        };
        queue.push_back(id);
        self.bitmap |= Self::mask_for(level);
        self.len += 1;
    }

    pub fn pop_highest(&mut self) -> Option<(ThreadId, Priority)> {
        let level = self.highest_level()?;
        let queue = self.queues.get_mut(level)?;
        let id = queue.pop_front()?;
        let drained = queue.is_empty();
        if drained {
            self.bitmap &= !Self::mask_for(level);
        }
        self.len = self.len.saturating_sub(1);
        Some((id, Self::level_priority(level)?))
    }

    /// Удаляет все записи `id` на известном уровне приоритета.
    pub fn remove(&mut self, id: ThreadId, priority: Priority) -> bool {
        let level = self.priority_index(priority);
        let Some(queue) = self.queues.get_mut(level) else {
            return false;
        };
        let before = queue.len();
        queue.retain(|queued| *queued != id);
        let removed = before - queue.len();
        let drained = queue.is_empty();
        if drained {
            self.bitmap &= !Self::mask_for(level);
        }
        self.len = self.len.saturating_sub(removed);
        removed != 0
    }

    /// Число потоков в очереди. Мера загрузки CPU при выборе места для
    /// нового потока и при перераспределении между CPU.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Старший по приоритету поток, принятый `accept`. Порядок обхода тот же,
    /// что у [`Self::pop_highest`], а очередь остаётся нетронутой.
    pub fn highest_matching(&self, accept: impl Fn(ThreadId) -> bool) -> Option<QueuedThread> {
        let mut levels = self.bitmap;
        while levels != 0 {
            let level = levels.leading_zeros() as usize;
            levels &= !Self::mask_for(level);
            let queue = self.queues.get(level)?;
            if let Some(thread) = queue.iter().copied().find(|id| accept(*id)) {
                return Some(QueuedThread {
                    thread,
                    priority: Self::level_priority(level)?,
                });
            }
        }
        None
    }

    /// Число потоков очереди, принятых `accept`.
    pub fn count_matching(&self, accept: impl Fn(ThreadId) -> bool) -> usize {
        self.queues
            .iter()
            .flatten()
            .filter(|id| accept(**id))
            .count()
    }

    pub fn peek_highest_priority(&self) -> Option<Priority> {
        Self::level_priority(self.highest_level()?)
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

    fn level_priority(level: usize) -> Option<Priority> {
        u8::try_from(level).ok().map(Priority::new)
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

        assert_eq!(queue.len(), 3, "length is the CPU load used for selection");
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
        assert_eq!(queue.len(), 0, "an empty queue never goes negative");
    }

    #[test]
    fn highest_matching_walks_levels_in_selection_order() {
        let mut queue = ReadyQueue::new(4);
        let rejected = thread(1);
        let accepted = thread(2);
        queue.push(rejected, Priority::new(0));
        queue.push(accepted, Priority::new(2));

        assert_eq!(
            queue.highest_matching(|id| id != rejected),
            Some(QueuedThread {
                thread: accepted,
                priority: Priority::new(2)
            })
        );
        assert_eq!(queue.highest_matching(|_| false), None);
        assert_eq!(queue.len(), 2, "lookup leaves the queue untouched");
    }

    #[test]
    fn remove_drops_every_matching_entry_and_updates_level() {
        let mut queue = ReadyQueue::new(4);
        let victim = thread(1);
        let other = thread(2);
        queue.push(victim, Priority::new(1));
        queue.push(other, Priority::new(1));
        queue.push(victim, Priority::new(1));

        assert!(queue.remove(victim, Priority::new(1)));
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.pop_highest(), Some((other, Priority::new(1))),);
        assert!(queue.is_empty());
    }
}
