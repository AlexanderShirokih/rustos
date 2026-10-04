//! E2E проверка `ResourceSplit` из EL0: бюджет производного ресурса
//! возвращается исходному его закрытием, а потомок, стартованный с явным
//! `resource_h`, метерится именно им. Границу самого производного бюджета
//! снимает проба в `resource_exhaustion`.

use kernel_objects::{Process, Resource, Signal, ThreadEntry};
use kernel_tests::kernel_test;
use runtime::{MemoryAccess, Priority};
use syscall::{ProcessName, Rights, SyscallError};
use userland::USERLAND_PAGE_SIZE;

use crate::{
    fixture::Fixture,
    fixture_protocol::{
        OP_ALLOCATE_PAGES, OP_DUPLICATE_TRANSFER_CAP, OP_EXIT, REPLY_ACCESS_DENIED,
        REPLY_BUDGET_EXHAUSTED, REPLY_OK,
    },
};

const CHILD_BYTES: u64 = 8 * 1024;

const DEPTH_CEILING: usize = 8;

const FIXTURE_SLACK_BYTES: u64 = 256 * 1024;

const OVERSIZE_FACTOR: u64 = 4;

#[kernel_test]
fn split_chain_depth_is_capped() {
    let mut held = alloc::vec::Vec::new();
    held.push(Resource::self_resource().expect("self metering resource"));
    let denied = loop {
        kernel_tests::kassert!(held.len() <= DEPTH_CEILING + 1);
        let leaf = held.last().expect("chain is non-empty");
        match leaf.split(1) {
            Ok(next) => held.push(next),
            Err(error) => break error,
        }
    };
    kernel_tests::kassert!(matches!(
        denied,
        kernel_objects::Error::Syscall(SyscallError::ResourceExhausted)
    ));

    let leaf = held.last().expect("chain is non-empty");
    let region = leaf
        .create_virtual(USERLAND_PAGE_SIZE, MemoryAccess::RW)
        .expect("leaf budget survives the refused split");
    drop(region);
}

#[kernel_test]
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

#[kernel_test]
fn child_started_with_a_resource_is_metered_by_it() {
    let metered = Fixture::spawn_with_slack(FIXTURE_SLACK_BYTES);
    let oversized = metered.budget_bytes().div_ceil(USERLAND_PAGE_SIZE) * OVERSIZE_FACTOR;

    let inherited = Fixture::spawn();
    kernel_tests::kassert_eq!(
        inherited.request_status(OP_ALLOCATE_PAGES, oversized, &[]),
        REPLY_OK
    );
    kernel_tests::kassert_eq!(inherited.request_status(OP_EXIT, 0, &[]), REPLY_OK);

    kernel_tests::kassert_eq!(
        metered.request_status(OP_ALLOCATE_PAGES, oversized, &[]),
        REPLY_BUDGET_EXHAUSTED
    );
    kernel_tests::kassert_eq!(metered.request_status(OP_EXIT, 0, &[]), REPLY_OK);
}

#[kernel_test]
fn child_stack_is_charged_to_the_image_budget() {
    let denied = Fixture::try_spawn_with_slack(0);
    kernel_tests::kassert_eq!(
        denied.err(),
        Some(kernel_objects::Error::Syscall(
            SyscallError::ResourceExhausted
        ))
    );
}

#[kernel_test]
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
    kernel_tests::kassert!(matches!(
        error,
        Some(kernel_objects::Error::Syscall(SyscallError::AccessDenied))
    ));

    let bootstrap = Signal::create().expect("signal create");
    let error = process.start(entry, bootstrap, Some(&resource)).err();
    kernel_tests::kassert!(matches!(
        error,
        Some(kernel_objects::Error::Syscall(SyscallError::WrongType))
    ));
}

#[kernel_test]
fn granted_resource_rights_shrink_to_the_passing_handle() {
    let parent = Resource::self_resource().expect("self metering resource");
    let untransferable = parent
        .duplicate(Rights::WRITE | Rights::DUPLICATE, None)
        .expect("resource capability without TRANSFER");
    let child = Fixture::spawn_on(&untransferable);
    kernel_tests::kassert_eq!(
        child.request_status(OP_DUPLICATE_TRANSFER_CAP, 0, &[]),
        REPLY_ACCESS_DENIED
    );
    kernel_tests::kassert_eq!(child.request_status(OP_EXIT, 0, &[]), REPLY_OK);

    let entitled = Fixture::spawn();
    kernel_tests::kassert_eq!(
        entitled.request_status(OP_DUPLICATE_TRANSFER_CAP, 0, &[]),
        REPLY_OK
    );
    kernel_tests::kassert_eq!(entitled.request_status(OP_EXIT, 0, &[]), REPLY_OK);
}

#[kernel_test]
fn a_split_of_one_byte_takes_a_whole_page() {
    let origin = Resource::self_resource().expect("self metering resource");
    let parent = origin.split(1).expect("one byte budget");

    let child = parent.split(1).expect("split of the whole budget");
    kernel_tests::kassert!(matches!(
        parent.split(1).err(),
        Some(kernel_objects::Error::Syscall(
            SyscallError::ResourceExhausted
        ))
    ));

    drop(child);
    kernel_tests::kassert!(parent.split(1).is_ok());
}

#[kernel_test]
fn split_of_zero_bytes_is_rejected() {
    let parent = Resource::self_resource().expect("self metering resource");

    kernel_tests::kassert!(matches!(
        parent.split(0).err(),
        Some(kernel_objects::Error::Syscall(
            SyscallError::InvalidArgument
        ))
    ));
}
