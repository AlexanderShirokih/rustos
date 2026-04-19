use alloc::collections::VecDeque;
use core::array;

use drivers_common::services::scheduler::{Priority, ThreadId};

const fn validate_priority_levels<const PRIO: usize>() {
    assert!(PRIO > 0 && PRIO <= 32);
}

pub struct ReadyQueue<const PRIO: usize> {
    bitmap: u32,
    queues: [VecDeque<ThreadId>; PRIO],
}

impl<const PRIO: usize> ReadyQueue<PRIO> {
    pub fn new() -> Self {
        const { validate_priority_levels::<PRIO>() };

        Self {
            bitmap: 0,
            queues: array::from_fn(|_| VecDeque::new()),
        }
    }

    pub fn push(&mut self, id: ThreadId, priority: Priority) {
        let level = Self::priority_index(priority);
        self.queues[level].push_back(id);
        self.bitmap |= Self::mask_for(level);
    }

    pub fn pop_highest(&mut self) -> Option<(ThreadId, Priority)> {
        let level = self.highest_level()?;
        let id = self.queues[level].pop_front()?;
        if self.queues[level].is_empty() {
            self.bitmap &= !Self::mask_for(level);
        }
        let priority = Priority::new(level as u8).expect("bitmap level must map to priority");
        Some((id, priority))
    }

    pub fn peek_highest_priority(&self) -> Option<Priority> {
        let level = self.highest_level()?;
        Priority::new(level as u8)
    }

    pub fn is_empty(&self) -> bool {
        self.bitmap == 0
    }

    fn highest_level(&self) -> Option<usize> {
        if self.bitmap == 0 {
            return None;
        }

        let level = self.bitmap.leading_zeros() as usize;
        if level < PRIO { Some(level) } else { None }
    }

    fn priority_index(priority: Priority) -> usize {
        let level = priority.raw() as usize;
        assert!(level < PRIO, "priority level {level} exceeds queue size {PRIO}");
        level
    }

    fn mask_for(level: usize) -> u32 {
        1u32 << (31 - level)
    }
}

impl<const PRIO: usize> Default for ReadyQueue<PRIO> {
    fn default() -> Self {
        Self::new()
    }
}
