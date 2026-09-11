#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_startup::component_manifest;
use filesystem_abi::{AccessMode, FileSystemError, NodeKind, OpenFlags, file_system};
use ipc::wire::Str;
use kernel_objects::{MemoryRegion, SessionTransport};
use runtime::{Timeout, UserMemFlags, install_log, log_fmt, log_line, process_exit};
use syscall::{Handle, kind};

const LOG_TAG: &str = "area";

const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const BUFFER_SIZE: u32 = 4096;

const RECORD: &str = "/sample";
const RECORD_BODY: &[u8] = b"component area of the userland image\n";

const ESCAPE_RELATIVE: &str = "/../testrunner/sample";
const ESCAPE_ABSOLUTE: &str = "/boot/testrunner/sample";

const NO_STARTUP_EXIT_CODE: u64 = 1;
const READ_EXIT_CODE: u64 = 2;
const NO_BUFFER_EXIT_CODE: u64 = 3;
const RECORD_UNREADABLE_EXIT_CODE: u64 = 4;
const AREA_LEAKS_EXIT_CODE: u64 = 5;
const PANIC_EXIT_CODE: u64 = 6;

component_manifest! {
    struct AreaStartup {
        requirements: { area: session(file_system::Protocol) },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    let Ok(started) = AreaStartup::start(parent) else {
        process_exit(NO_STARTUP_EXIT_CODE);
    };
    if let Some(log) = started.log {
        install_log(log.into_handle().into_raw(), LOG_TAG);
    }

    let area = started.requirements.area.client().with_wait(CALL_TIMEOUT);
    let code = inspect(&area);
    if code == READ_EXIT_CODE {
        log_line("area read");
    }
    process_exit(code)
}

fn inspect(client: &file_system::Client<SessionTransport>) -> u64 {
    let Ok(Ok(cap)) = client.acquire_buffer(BUFFER_SIZE) else {
        return NO_BUFFER_EXIT_CODE;
    };
    let region = MemoryRegion::from(cap);
    let Ok(buffer) = region.map_full(UserMemFlags::ReadWrite) else {
        return NO_BUFFER_EXIT_CODE;
    };

    let Some(path) = Str::new(RECORD) else {
        return RECORD_UNREADABLE_EXIT_CODE;
    };
    let Ok(Ok(opened)) = client.open(path, AccessMode::Read, OpenFlags::NONE) else {
        return RECORD_UNREADABLE_EXIT_CODE;
    };
    if opened.kind != NodeKind::File || opened.size != RECORD_BODY.len() as u64 {
        return RECORD_UNREADABLE_EXIT_CODE;
    }
    let Ok(Ok(count)) = client.read(opened.fd, 0, BUFFER_SIZE) else {
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
    READ_EXIT_CODE
}

fn escapes(
    client: &file_system::Client<SessionTransport>,
    text: &str,
    refused: FileSystemError,
) -> bool {
    let Some(path) = Str::new(text) else {
        return true;
    };
    client.open(path, AccessMode::Read, OpenFlags::NONE) != Ok(Err(refused))
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    process_exit(PANIC_EXIT_CODE)
}
