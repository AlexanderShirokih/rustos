mod common;

use std::num::NonZeroU32;

use scheduler::{Priority, ThreadId, ready_queue::ReadyQueue};

fn thread_id(raw: u32) -> ThreadId {
    ThreadId::new(NonZeroU32::new(raw).expect("thread id must be non-zero"))
}

#[test]
fn pop_highest_prefers_lower_priority_number() {
    let mut queue = ReadyQueue::new(32);
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
    let mut queue = ReadyQueue::new(32);
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
    let mut queue = ReadyQueue::new(8);
    let lowest = thread_id(1);
    queue.push(lowest, Priority::new(7));
    let (id, prio) = queue.pop_highest().expect("queue must not be empty");
    assert_eq!(id, lowest);
    assert_eq!(prio, Priority::new(7));
}

#[test]
fn bitmap_stays_in_sync_with_per_level_queues() {
    // Регресс на согласованность bitmap/queues: `peek_highest_priority`,
    // `is_empty` и порядок `pop_highest` должны соответствовать фактическому
    // содержимому очередей при перемешанных push/pop разных приоритетов.
    let mut queue = ReadyQueue::new(32);
    let high_a = thread_id(1);
    let high_b = thread_id(2);
    let low = thread_id(3);

    queue.push(low, Priority::new(20));
    queue.push(high_a, Priority::new(5));
    queue.push(high_b, Priority::new(5));

    assert_eq!(queue.peek_highest_priority(), Some(Priority::new(5)));
    assert!(!queue.is_empty());

    // Уровень 5 опустошается за два pop (FIFO), и только потом bitmap
    // переключает peek на уровень 20.
    assert_eq!(queue.pop_highest(), Some((high_a, Priority::new(5))));
    assert_eq!(queue.peek_highest_priority(), Some(Priority::new(5)));
    assert_eq!(queue.pop_highest(), Some((high_b, Priority::new(5))));

    assert_eq!(queue.peek_highest_priority(), Some(Priority::new(20)));
    assert_eq!(queue.pop_highest(), Some((low, Priority::new(20))));

    assert!(queue.is_empty());
    assert_eq!(queue.peek_highest_priority(), None);
    assert_eq!(queue.pop_highest(), None);
}
