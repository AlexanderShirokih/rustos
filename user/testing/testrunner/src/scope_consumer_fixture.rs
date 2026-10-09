#![no_std]
#![no_main]

use component_startup::{Requirements as _, component_manifest};
use filesystem_abi::{AccessMode, FileSystemError, NodeKind, OpenFlags, file_system, namespace};
use ipc::CallError;
use kernel_objects::{MemoryRegion, SessionTransport};
use runtime::{Timeout, UserMemFlags, log_line};
use syscall::{Handle, kind};

mod failed;

use failed::Failed;

const FILESYSTEM_REQUEST_TIMEOUT: Timeout = Timeout::from_secs(2);

const BUFFER_SIZE: u32 = 4096;

const RECORD: &str = "/sample";
const RECORD_BODY: &[u8] = b"component area of the userland image\n";

const ESCAPE_RELATIVE: &str = "/../testrunner/sample";
const ESCAPE_ABSOLUTE: &str = "/testrunner/sample";

component_manifest! {
    struct AreaStartup {
        requirements: { area: session(namespace::Protocol) },
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Failed> {
    let area = AreaStartup::from_parent(parent)
        .area
        .open()
        .map_err(|_| Failed)?;
    let root = "/boot/scope-consumer-fixture";
    let session = area
        .client()
        .with_wait(Timeout::from_secs(5))
        .resolve(root)
        .map_err(|_| Failed)?;
    let area = kernel_objects::Session::from_session(session)
        .client()
        .with_wait(FILESYSTEM_REQUEST_TIMEOUT);

    if inspect(&area) {
        log_line("area read");
    }
    Err(Failed)
}

fn inspect(client: &file_system::Client<SessionTransport>) -> bool {
    let Ok(cap) = client.acquire_buffer(BUFFER_SIZE) else {
        return false;
    };
    let region = MemoryRegion::from(cap);
    let Ok(buffer) = region.map_full(UserMemFlags::ReadWrite) else {
        return false;
    };

    let Ok(opened) = client.open(RECORD, AccessMode::Read, OpenFlags::NONE) else {
        return false;
    };
    if opened.kind != NodeKind::File || opened.size != RECORD_BODY.len() as u64 {
        return false;
    }
    let Ok(count) = client.read(opened.fd, 0, BUFFER_SIZE) else {
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
    client.open(text, AccessMode::Read, OpenFlags::NONE) != Err(CallError::Remote(refused))
}
