#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_startup::component_manifest;
use filesystem_abi::{AccessMode, FileSystemError, NodeKind, OpenFlags, file_system};
use ipc::wire::Str;
use kernel_objects::{MemoryRegion, SessionTransport};
use runtime::{Timeout, UserMemFlags, log_fmt, log_line, process_exit, start_component};
use syscall::{Handle, kind};

const LOG_TAG: &str = "area";

const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const BUFFER_SIZE: u32 = 4096;

const RECORD: &str = "/sample";
const RECORD_BODY: &[u8] = b"component area of the userland image\n";

const ESCAPE_RELATIVE: &str = "/../testrunner/sample";
const ESCAPE_ABSOLUTE: &str = "/testrunner/sample";

const RESTART_EXIT_CODE: u64 = 1;

component_manifest! {
    struct AreaStartup {
        requirements: { area: session(file_system::Protocol) },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    start_component(parent, LOG_TAG);
    let Ok(requirements) = AreaStartup::request(parent) else {
        process_exit(RESTART_EXIT_CODE);
    };

    let area = requirements.area.client().with_wait(CALL_TIMEOUT);
    if inspect(&area) {
        log_line("area read");
    }
    process_exit(RESTART_EXIT_CODE)
}

fn inspect(client: &file_system::Client<SessionTransport>) -> bool {
    let Ok(Ok(cap)) = client.acquire_buffer(BUFFER_SIZE) else {
        return false;
    };
    let region = MemoryRegion::from(cap);
    let Ok(buffer) = region.map_full(UserMemFlags::ReadWrite) else {
        return false;
    };

    let Some(path) = Str::new(RECORD) else {
        return false;
    };
    let Ok(Ok(opened)) = client.open(path, AccessMode::Read, OpenFlags::NONE) else {
        return false;
    };
    if opened.kind != NodeKind::File || opened.size != RECORD_BODY.len() as u64 {
        return false;
    }
    let Ok(Ok(count)) = client.read(opened.fd, 0, BUFFER_SIZE) else {
        return false;
    };
    let mut body = [0; RECORD_BODY.len()];
    if count as usize != RECORD_BODY.len() || buffer.copy_out(&mut body).is_err() {
        return false;
    }
    if body != RECORD_BODY {
        return false;
    }

    if escapes(client, ESCAPE_RELATIVE, FileSystemError::InvalidPath)
        || escapes(client, ESCAPE_ABSOLUTE, FileSystemError::NotFound)
    {
        return false;
    }
    true
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
    process_exit(RESTART_EXIT_CODE)
}
