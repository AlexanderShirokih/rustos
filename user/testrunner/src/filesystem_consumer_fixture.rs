#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_client::{ReceivedStartup, component_manifest};
use filesystem_abi::{AccessMode, NodeKind, OpenFlags, file_system};
use ipc::wire::Str;
use kernel_objects::{MemoryRegion, OwnedHandle};
use runtime::{PortTransport, Timeout, UserMemFlags, install_log, log_fmt, thread_exit};
use syscall::Handle;

const LOG_TAG: &str = "files";

const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const BUFFER_SIZE: u32 = 4096;

const RECORD: &str = "/boot/testrunner/sample";
const RECORD_BODY: &[u8] = b"hello from the userland image\n";

const OK_EXIT_CODE: u64 = 0;
const NO_STARTUP_EXIT_CODE: u64 = 1;
const NO_BUFFER_EXIT_CODE: u64 = 3;
const RECORD_UNREADABLE_EXIT_CODE: u64 = 4;
const PANIC_EXIT_CODE: u64 = 6;

component_manifest! {
    struct FileSystemConsumerStartup {
        slots: { files: service(protocol = "FileSystem", instance = "filesystem-imagefs") },
    }
}

/// Программа-потребитель файловой системы: читает запись образа сессией сервисного слота.
/// `startup` приходит в x0 как HandleId одноразового startup-порта.
#[unsafe(no_mangle)]
pub extern "C" fn _start(startup: Handle) -> ! {
    let Ok(ReceivedStartup { log, resources }) =
        FileSystemConsumerStartup::receive(OwnedHandle::from_handle(startup))
    else {
        thread_exit(NO_STARTUP_EXIT_CODE);
    };
    install_log(log.into_raw(), LOG_TAG);
    drop(resources.control);
    drop(resources.service);

    let files = resources.files;
    thread_exit(read_record(
        &file_system::Client::new(PortTransport::client(&files)).with_wait(CALL_TIMEOUT),
    ))
}

fn read_record(client: &file_system::Client<PortTransport>) -> u64 {
    let Ok(mut response) = client.acquire_buffer(BUFFER_SIZE) else {
        return NO_BUFFER_EXIT_CODE;
    };
    let Ok(capability) = response.value else {
        return NO_BUFFER_EXIT_CODE;
    };
    let Some(cap) = response.claim(capability) else {
        return NO_BUFFER_EXIT_CODE;
    };
    let region = MemoryRegion::from_handle(cap.into());
    let Ok(buffer) = region.map_full(UserMemFlags::ReadWrite) else {
        return NO_BUFFER_EXIT_CODE;
    };

    let Some(path) = Str::new(RECORD) else {
        return RECORD_UNREADABLE_EXIT_CODE;
    };
    let Ok(Ok(opened)) = client
        .open(path, AccessMode::Read, OpenFlags::NONE)
        .map(|response| response.value)
    else {
        return RECORD_UNREADABLE_EXIT_CODE;
    };
    if opened.kind != NodeKind::File || opened.size != RECORD_BODY.len() as u64 {
        return RECORD_UNREADABLE_EXIT_CODE;
    }
    let Ok(Ok(count)) = client
        .read(opened.fd, 0, BUFFER_SIZE)
        .map(|response| response.value)
    else {
        return RECORD_UNREADABLE_EXIT_CODE;
    };
    let mut body = [0; RECORD_BODY.len()];
    if count as usize != RECORD_BODY.len() || buffer.copy_out(&mut body).is_err() {
        return RECORD_UNREADABLE_EXIT_CODE;
    }
    if body != RECORD_BODY {
        return RECORD_UNREADABLE_EXIT_CODE;
    }
    OK_EXIT_CODE
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    thread_exit(PANIC_EXIT_CODE)
}
