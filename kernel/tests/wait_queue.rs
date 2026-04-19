mod common;

use std::num::NonZeroU32;

use drivers_common::services::scheduler::ThreadId;
use kernel::sched::wait_queue::{SleepEntry, SleepQueue, WaitQueue};

fn thread_id(raw: u32) -> ThreadId {
    ThreadId::new(NonZeroU32::new(raw).expect("thread id must be non-zero"))
}

#[test]
fn wait_queue_is_fifo() {
    let mut queue = WaitQueue::new();
    let first = thread_id(1);
    let second = thread_id(2);

    queue.push(first);
    queue.push(second);

    assert_eq!(queue.pop(), Some(first));
    assert_eq!(queue.pop(), Some(second));
    assert!(queue.is_empty());
}

#[test]
fn sleep_queue_returns_earliest_deadline_first() {
    let mut queue = SleepQueue::new();
    let early = SleepEntry {
        wakeup_at_ns: 50,
        thread_id: thread_id(1),
    };
    let late = SleepEntry {
        wakeup_at_ns: 100,
        thread_id: thread_id(2),
    };

    queue.push(late);
    queue.push(early);

    assert_eq!(queue.pop(), Some(early));
    assert_eq!(queue.pop(), Some(late));
}
