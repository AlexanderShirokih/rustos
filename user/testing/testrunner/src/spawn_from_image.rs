//! E2E спавна из реального userland-образа.

use kernel_objects::{Resource, Signal};
use kernel_tests::kernel_test;
use runtime::Timeout;
use syscall::{Rights, SIGNALED, SyscallError};
use userland::{Entry, Segment, SegmentPermissions, USERLAND_PAGE_SIZE};
use userland_image::component_name;
use userland_loader::load_entry;

const REPLY_HOLDER: &str = "/reply-holder/program";
const ROLLBACK_PROBE_NAME: &str = "load-image-rollback";

const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);
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
    let invalid = Entry {
        path: "/load-image-rollback",
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
                load_entry(&invalid, ROLLBACK_PROBE_NAME, &resource, initial)
            },
            SyscallError::InvalidArgument,
            &timer,
        );
    }
}

#[kernel_test]
fn start_error_reclaims_each_loaded_process() {
    let child = crate::fixture::program(REPLY_HOLDER);

    let root = Resource::self_resource().expect("self metering resource");
    let write_only = root
        .duplicate(Rights::WRITE, None)
        .expect("resource without duplicate right");
    let timer = Signal::create().expect("cleanup retry timer");

    for _ in 0..PROCESS_CREATE_ROLLBACK_ATTEMPTS {
        fail_load_when_process_returns(
            || {
                let initial = Signal::create().expect("initial capability");
                load_entry(&child, component_name(REPLY_HOLDER), &write_only, initial)
            },
            SyscallError::AccessDenied,
            &timer,
        );
    }
}

/// Сервер принимает `call` и уходит, не ответив: вызыватель обязан быть
/// разбужен отказом.
#[kernel_test]
fn caller_wakes_when_server_dies_holding_reply() {
    use kernel_objects::{Error, Port};
    use syscall::SyscallError;

    let service = Port::create_service(None).expect("port create");
    let _process = crate::fixture::spawn_entry(REPLY_HOLDER, service.server);

    let mut out = [0u8; 8];
    // Кадр уже принят, поэтому вызыватель снят с очереди порта. Его будит снос
    // reply-хэндла вместе с таблицей ушедшего сервера, а не пустое множество
    // получателей. Тайм-аут страхует от вечной парковки, исход - `PeerClosed`.
    let result = service.client.call_bytes(b"x", &mut out, CALL_TIMEOUT);
    kernel_tests::kassert!(matches!(
        result,
        Err(Error::Syscall(SyscallError::PeerClosed))
    ));
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
