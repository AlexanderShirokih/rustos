mod common;

use std::num::NonZeroU32;

use drivers_common::services::scheduler::{Priority, ThreadId};
use kernel::sched::ready_queue::ReadyQueue;

fn thread_id(raw: u32) -> ThreadId {
    ThreadId::new(NonZeroU32::new(raw).expect("thread id must be non-zero"))
}

#[test]
fn pop_highest_prefers_lower_priority_number() {
    let mut queue = ReadyQueue::<32>::new();
    let low = thread_id(1);
    let high = thread_id(2);

    queue.push(low, Priority::new(10));
    queue.push(high, Priority::new(2));

    let (id, priority) = queue.pop_highest().expect("queue must not be empty");
    assert_eq!(id, high);
    assert_eq!(priority, Priority::new(2));
}

#[test]
fn fifo_is_preserved_within_same_priority() {
    let mut queue = ReadyQueue::<32>::new();
    let first = thread_id(1);
    let second = thread_id(2);
    let priority = Priority::normal();

    queue.push(first, priority);
    queue.push(second, priority);

    assert_eq!(queue.pop_highest().map(|(id, _)| id), Some(first));
    assert_eq!(queue.pop_highest().map(|(id, _)| id), Some(second));
    assert!(queue.is_empty());
}

#[test]
fn arbitrary_priority_levels_supported() {
    let mut queue = ReadyQueue::<8>::new();
    let lowest = thread_id(1);
    queue.push(lowest, Priority::new(7));
    let (id, prio) = queue.pop_highest().expect("queue must not be empty");
    assert_eq!(id, lowest);
    assert_eq!(prio, Priority::new(7));
}
