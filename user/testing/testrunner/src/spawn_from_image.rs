//! E2E спавна из реального userland-образа.

use kernel_objects::{Resource, Signal};
use kernel_tests::kernel_test;
use runtime::Timeout;
use syscall::{ProcessName, Rights, SIGNALED, SyscallError};
use userland::{Program, Segment, SegmentPermissions, USERLAND_PAGE_SIZE};
use userland_image::component_name;
use userland_loader::load_program;

use crate::fixture::FIXTURE_ENTRY;

const ROLLBACK_PROBE_NAME: &str = "load-image-rollback";

const PROCESS_CLEANUP_RETRY_ATTEMPTS: usize = 64;
const PROCESS_CLEANUP_RETRY: Timeout = Timeout::from_millis(1);
const PROCESS_CREATE_ROLLBACK_ATTEMPTS: usize = 80;

#[kernel_test]
fn load_image_error_reclaims_each_created_process() {
    const BASE: u64 = 0x4000_0000;
    let segments = [
        Segment {
            va_base: BASE,
            mem_size: USERLAND_PAGE_SIZE,
            permissions: SegmentPermissions::ReadExecute,
            bytes: &[],
        },
        Segment {
            va_base: BASE,
            mem_size: USERLAND_PAGE_SIZE,
            permissions: SegmentPermissions::ReadWrite,
            bytes: &[],
        },
    ];
    let invalid = Program {
        manifest: &[],
        entry_va: BASE,
        stack_size: USERLAND_PAGE_SIZE,
        segments: &segments,
    };
    let resource = Resource::self_resource().expect("self metering resource");
    let timer = Signal::create().expect("cleanup retry timer");

    for _ in 0..PROCESS_CREATE_ROLLBACK_ATTEMPTS {
        fail_load_when_process_returns(
            || {
                let initial = Signal::create().expect("initial capability");
                load_program(
                    &invalid,
                    ProcessName::new(ROLLBACK_PROBE_NAME).expect("fixture process name"),
                    &resource,
                    initial,
                )
            },
            SyscallError::InvalidArgument,
            &timer,
        );
    }
}

#[kernel_test]
fn start_error_reclaims_each_loaded_process() {
    let child = crate::fixture::program(FIXTURE_ENTRY);

    let root = Resource::self_resource().expect("self metering resource");
    let write_only = root
        .duplicate(Rights::WRITE, None)
        .expect("resource without duplicate right");
    let timer = Signal::create().expect("cleanup retry timer");

    for _ in 0..PROCESS_CREATE_ROLLBACK_ATTEMPTS {
        fail_load_when_process_returns(
            || {
                let initial = Signal::create().expect("initial capability");
                load_program(
                    &child,
                    ProcessName::new(component_name(FIXTURE_ENTRY)).expect("fixture process name"),
                    &write_only,
                    initial,
                )
            },
            SyscallError::AccessDenied,
            &timer,
        );
    }
}

fn fail_load_when_process_returns(
    load: impl Fn() -> kernel_objects::Result<kernel_objects::Process>,
    expected: SyscallError,
    timer: &Signal,
) {
    for _ in 0..PROCESS_CLEANUP_RETRY_ATTEMPTS {
        match load() {
            Err(kernel_objects::Error::Syscall(error)) if error == expected => return,
            Err(kernel_objects::Error::Syscall(
                SyscallError::OutOfMemory | SyscallError::ResourceExhausted,
            )) => {
                let _ = timer.wait(SIGNALED, PROCESS_CLEANUP_RETRY);
            }
            Err(error) => panic!("unexpected load failure: {error:?}"),
            Ok(_) => panic!("load with an injected failure succeeded"),
        }
    }
    panic!("created process was not returned")
}
