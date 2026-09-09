#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_startup::{ReceivedStartup, component_manifest};
use filesystem_abi::{AccessMode, FileSystemError, NodeKind, OpenFlags, file_system};
use ipc::wire::Str;
use kernel_objects::{MemoryRegion, Port, kind};
use runtime::{PortTransport, Timeout, UserMemFlags, install_log, log_fmt, thread_exit};
use syscall::Handle;

const LOG_TAG: &str = "area";

const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const BUFFER_SIZE: u32 = 4096;

const RECORD: &str = "/sample";
const RECORD_BODY: &[u8] = b"component area of the userland image\n";

const ESCAPE_RELATIVE: &str = "/../testrunner/sample";
const ESCAPE_ABSOLUTE: &str = "/boot/testrunner/sample";

const OK_EXIT_CODE: u64 = 0;
const NO_STARTUP_EXIT_CODE: u64 = 1;
const NO_BUFFER_EXIT_CODE: u64 = 3;
const RECORD_UNREADABLE_EXIT_CODE: u64 = 4;
const AREA_LEAKS_EXIT_CODE: u64 = 5;
const PANIC_EXIT_CODE: u64 = 6;

component_manifest! {
    struct AreaStartup {
        slots: { area: directory(instance = "filesystem-imagefs") },
    }
}

/// Программа-потребитель файловой области читает свою запись сессией каталога
/// и проверяет, что путь наружу область не выражает.
/// `startup` - серверная сторона startup-порта, аргумент точки входа.
#[unsafe(no_mangle)]
pub extern "C" fn _start(startup: Handle<kind::Port>) -> ! {
    let Ok(ReceivedStartup { log, resources }) = AreaStartup::receive(Port::from_handle(startup))
    else {
        thread_exit(NO_STARTUP_EXIT_CODE);
    };
    install_log(log.into_raw(), LOG_TAG);
    drop(resources.startup);

    let area = resources.area;
    thread_exit(inspect(
        &file_system::Client::new(PortTransport::client(&area)).with_wait(CALL_TIMEOUT),
    ))
}

fn inspect(client: &file_system::Client<PortTransport>) -> u64 {
    let Ok(mut response) = client.acquire_buffer(BUFFER_SIZE) else {
        return NO_BUFFER_EXIT_CODE;
    };
    let Ok(capability) = response.value else {
        return NO_BUFFER_EXIT_CODE;
    };
    let Some(cap) = response.claim(capability) else {
        return NO_BUFFER_EXIT_CODE;
    };
    let region = MemoryRegion::from(cap);
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

    if escapes(client, ESCAPE_RELATIVE, FileSystemError::InvalidPath)
        || escapes(client, ESCAPE_ABSOLUTE, FileSystemError::NotFound)
    {
        return AREA_LEAKS_EXIT_CODE;
    }
    OK_EXIT_CODE
}

fn escapes(
    client: &file_system::Client<PortTransport>,
    text: &str,
    refused: FileSystemError,
) -> bool {
    let Some(path) = Str::new(text) else {
        return true;
    };
    client
        .open(path, AccessMode::Read, OpenFlags::NONE)
        .map(|response| response.value)
        != Ok(Err(refused))
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    thread_exit(PANIC_EXIT_CODE)
}
