//! E2E проверка `ResourceSplit` из EL0: бюджет производного ресурса
//! возвращается исходному его закрытием, а потомок, стартованный с явным
//! `resource_h`, метерится именно им и не оплачивает сегменты без записи из
//! страниц образа. Границу самого производного бюджета снимает проба в
//! `resource_exhaustion`.

use kernel_objects::{Process, Resource, Signal, ThreadEntry};
use runtime::{MemoryAccess, Priority};
use syscall::{ProcessName, Rights, SyscallError};
use userland::USERLAND_PAGE_SIZE;

use crate::{
    composition::image,
    fixture::{FIXTURE_ENTRY, Fixture, process_bytes, program, shared_bytes},
    fixture_protocol::{
        OP_ALLOCATE_PAGES, OP_DUPLICATE_TRANSFER_CAP, OP_EXIT, REPLY_ACCESS_DENIED,
        REPLY_BUDGET_EXHAUSTED, REPLY_OK,
    },
};

const CHILD_BYTES: u64 = 8 * 1024;

const DEPTH_CEILING: usize = 8;

const FIXTURE_SLACK_BYTES: u64 = 256 * 1024;

const OVERSIZE_FACTOR: u64 = 4;

#[test_harness::test]
fn split_chain_depth_is_capped() {
    let mut held = alloc::vec::Vec::new();
    held.push(Resource::self_resource().expect("self metering resource"));
    let denied = loop {
        test_harness::check!(held.len() <= DEPTH_CEILING + 1);
        let leaf = held.last().expect("chain is non-empty");
        match leaf.split(1) {
            Ok(next) => held.push(next),
            Err(error) => break error,
        }
    };
    test_harness::check!(matches!(
        denied,
        kernel_objects::Error::Syscall(SyscallError::ResourceExhausted)
    ));

    let leaf = held.last().expect("chain is non-empty");
    let region = leaf
        .create_virtual(USERLAND_PAGE_SIZE, MemoryAccess::RW)
        .expect("leaf budget survives the refused split");
    drop(region);
}

#[test_harness::test]
fn closing_the_derived_resource_returns_its_budget() {
    let parent = Resource::self_resource().expect("self metering resource");

    for _ in 0..64 {
        let child = parent.split(CHILD_BYTES).expect("split");
        let region = child
            .create_virtual(USERLAND_PAGE_SIZE, MemoryAccess::RW)
            .expect("region within the derived budget");
        drop(region);
        drop(child);
    }
}

#[test_harness::test]
fn child_started_with_a_resource_is_metered_by_it() {
    let metered = Fixture::spawn_with_slack(FIXTURE_SLACK_BYTES);
    let oversized = metered.budget_bytes().div_ceil(USERLAND_PAGE_SIZE) * OVERSIZE_FACTOR;

    let inherited = Fixture::spawn();
    test_harness::check_eq!(
        inherited.request_status(OP_ALLOCATE_PAGES, oversized, &[]),
        REPLY_OK
    );
    test_harness::check_eq!(inherited.request_status(OP_EXIT, 0, &[]), REPLY_OK);

    test_harness::check_eq!(
        metered.request_status(OP_ALLOCATE_PAGES, oversized, &[]),
        REPLY_BUDGET_EXHAUSTED
    );
    test_harness::check_eq!(metered.request_status(OP_EXIT, 0, &[]), REPLY_OK);
}

#[test_harness::test]
fn child_stack_is_charged_to_the_image_budget() {
    let denied = Fixture::try_spawn_with_slack(0);
    test_harness::check_eq!(
        denied.err(),
        Some(kernel_objects::Error::Syscall(
            SyscallError::ResourceExhausted
        ))
    );
}

#[test_harness::test]
fn image_pages_spare_the_child_budget_its_read_only_segments() {
    let entry = program(FIXTURE_ENTRY);
    let shared_pages = shared_bytes(&entry) / USERLAND_PAGE_SIZE;
    test_harness::check!(shared_pages > 0);
    let budget = process_bytes(&entry) + shared_bytes(&entry) + FIXTURE_SLACK_BYTES;

    let pages = image().pages();
    let copied = Fixture::spawn_split(budget, None).expect("spawn fixture without image pages");
    let first = Fixture::spawn_split(budget, Some(&pages)).expect("spawn first fixture");
    let second = Fixture::spawn_split(budget, Some(&pages)).expect("spawn second fixture");

    let copied_spare = spare_pages(&copied);
    let first_spare = spare_pages(&first);
    let second_spare = spare_pages(&second);
    test_harness::check_eq!(first_spare, copied_spare + shared_pages);
    test_harness::check_eq!(second_spare, copied_spare + shared_pages);

    for fixture in [copied, first, second] {
        test_harness::check_eq!(fixture.request_status(OP_EXIT, 0, &[]), REPLY_OK);
    }
}

#[test_harness::test]
fn starting_a_child_on_a_write_only_resource_is_denied() {
    let resource = Resource::self_resource().expect("self metering resource");
    let write_only = resource
        .duplicate(Rights::WRITE, None)
        .expect("write-only resource capability");
    let process = Process::create(ProcessName::new("resource-rights").expect("process name"))
        .expect("process create");
    let bootstrap = Signal::create().expect("signal create");

    let entry = ThreadEntry {
        entry_pc: 0,
        user_sp: 0,
        arg: 0,
        priority: Priority::new(0),
    };
    let error = process.start(entry, bootstrap, Some(&write_only)).err();
    test_harness::check!(matches!(
        error,
        Some(kernel_objects::Error::Syscall(SyscallError::AccessDenied))
    ));

    let bootstrap = Signal::create().expect("signal create");
    let error = process.start(entry, bootstrap, Some(&resource)).err();
    test_harness::check!(matches!(
        error,
        Some(kernel_objects::Error::Syscall(SyscallError::WrongType))
    ));
}

#[test_harness::test]
fn granted_resource_rights_shrink_to_the_passing_handle() {
    let parent = Resource::self_resource().expect("self metering resource");
    let untransferable = parent
        .duplicate(Rights::WRITE | Rights::DUPLICATE, None)
        .expect("resource capability without TRANSFER");
    let child = Fixture::spawn_on(&untransferable);
    test_harness::check_eq!(
        child.request_status(OP_DUPLICATE_TRANSFER_CAP, 0, &[]),
        REPLY_ACCESS_DENIED
    );
    test_harness::check_eq!(child.request_status(OP_EXIT, 0, &[]), REPLY_OK);

    let entitled = Fixture::spawn();
    test_harness::check_eq!(
        entitled.request_status(OP_DUPLICATE_TRANSFER_CAP, 0, &[]),
        REPLY_OK
    );
    test_harness::check_eq!(entitled.request_status(OP_EXIT, 0, &[]), REPLY_OK);
}

#[test_harness::test]
fn a_split_of_one_byte_takes_a_whole_page() {
    let origin = Resource::self_resource().expect("self metering resource");
    let parent = origin.split(1).expect("one byte budget");

    let child = parent.split(1).expect("split of the whole budget");
    test_harness::check!(matches!(
        parent.split(1).err(),
        Some(kernel_objects::Error::Syscall(
            SyscallError::ResourceExhausted
        ))
    ));

    drop(child);
    test_harness::check!(parent.split(1).is_ok());
}

#[test_harness::test]
fn split_of_zero_bytes_is_rejected() {
    let parent = Resource::self_resource().expect("self metering resource");

    test_harness::check!(matches!(
        parent.split(0).err(),
        Some(kernel_objects::Error::Syscall(
            SyscallError::InvalidArgument
        ))
    ));
}

pub(crate) fn spare_pages(fixture: &Fixture) -> u64 {
    let mut granted = 0;
    let mut refused = fixture.budget_bytes().div_ceil(USERLAND_PAGE_SIZE) + 1;
    while refused - granted > 1 {
        let probe = granted + (refused - granted) / 2;
        match fixture.request_status(OP_ALLOCATE_PAGES, probe, &[]) {
            REPLY_OK => granted = probe,
            REPLY_BUDGET_EXHAUSTED => refused = probe,
            reply => panic!("allocation probe answered {reply}"),
        }
    }
    granted
}
