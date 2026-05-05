mod common;

use std::num::NonZeroU32;

use drivers_common::services::scheduler::{Priority, ThreadId};
use main::sched::{
    CpuId, ProcessId, Thread, ThreadStack, ThreadStackAllocator, thread_table::ThreadTable,
};

use crate::common::{MockContext, MockStack};

fn build_thread(id: ThreadId, name: &'static str, priority: Priority) -> Thread<MockContext> {
    let process = ProcessId::new(NonZeroU32::new(1).expect("non-zero"));
    let stack: ThreadStack = MockStack::allocate(1).expect("mock stack allocation");
    Thread::new(
        id,
        process,
        CpuId::new(0),
        priority,
        MockContext,
        stack,
        name,
    )
}

#[test]
fn insert_get_and_remove_thread() {
    let mut table = ThreadTable::<MockContext>::new(4);

    let id = table
        .insert_with(|id| build_thread(id, "worker", Priority::normal()))
        .expect("insert must succeed");
    let inserted = table.get(id).expect("thread must be present");
    assert_eq!(inserted.id(), id);
    assert_eq!(inserted.name(), "worker");

    let removed = table.remove(id).expect("remove must return thread");
    assert_eq!(removed.id(), id);
    assert!(table.get(id).is_none());
}

#[test]
fn split_pair_mut_returns_distinct_threads() {
    let mut table = ThreadTable::<MockContext>::new(4);
    let first = table
        .insert_with(|id| build_thread(id, "first", Priority::normal()))
        .expect("first insert");
    let second = table
        .insert_with(|id| build_thread(id, "second", Priority::new(3)))
        .expect("second insert");

    let (first_ref, second_ref) = table
        .split_pair_mut(first, second)
        .expect("threads must be distinct");

    first_ref.set_time_slice_left(7);
    second_ref.set_time_slice_left(3);

    assert_eq!(table.get(first).expect("first thread").time_slice_left(), 7);
    assert_eq!(
        table.get(second).expect("second thread").time_slice_left(),
        3
    );
}

#[test]
fn full_table_returns_no_free_slots_error() {
    let mut table = ThreadTable::<MockContext>::new(1);
    let _ = table
        .insert_with(|id| build_thread(id, "a", Priority::normal()))
        .expect("first insert");
    let err = table
        .insert_with(|id| build_thread(id, "b", Priority::normal()))
        .unwrap_err();
    assert_eq!(
        err,
        drivers_common::services::scheduler::SpawnError::NoFreeThreadSlots
    );
}
