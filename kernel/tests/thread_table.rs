mod common;

use drivers_common::services::scheduler::Priority;
use kernel::sched::thread_table::ThreadTable;

use crate::common::make_thread;

#[test]
fn insert_get_and_remove_thread() {
    let mut table = ThreadTable::<crate::common::MockContext, 4>::new();
    let thread = make_thread("worker", Priority::normal());

    let id = table.insert(thread).expect("insert must succeed");
    let inserted = table.get(id).expect("thread must be present");
    assert_eq!(inserted.id(), id);
    assert_eq!(inserted.name(), "worker");

    let removed = table.remove(id).expect("remove must return thread");
    assert_eq!(removed.id(), id);
    assert!(table.get(id).is_none());
}

#[test]
fn split_pair_mut_returns_distinct_threads() {
    let mut table = ThreadTable::<crate::common::MockContext, 4>::new();
    let first = table
        .insert(make_thread("first", Priority::normal()))
        .expect("first insert");
    let second = table
        .insert(make_thread("second", Priority::new(3).expect("priority")))
        .expect("second insert");

    let (first_ref, second_ref) = table
        .split_pair_mut(first, second)
        .expect("threads must be distinct");

    first_ref.set_time_slice_left(7);
    second_ref.set_time_slice_left(3);

    assert_eq!(
        table.get(first).expect("first thread").time_slice_left(),
        7
    );
    assert_eq!(
        table.get(second).expect("second thread").time_slice_left(),
        3
    );
}
