#![no_std]
#![no_main]
#![allow(unsafe_code)]

use component_startup::{Requirements as _, component_manifest};
use filesystem_abi::{AccessMode, FileSystemError, NodeKind, OpenFlags, file_system, namespace};
use ipc::wire::Str;
use kernel_objects::{MemoryRegion, Port, SessionTransport};
use runtime::{Timeout, UserMemFlags, log_line, start_component};

mod fixture_start;

use fixture_start::{Failed, fixture_start};

const LOG_TAG: &str = "area";

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

fixture_start!(run);

fn run(parent: &Port) -> Result<(), Failed> {
    start_component(parent.as_raw(), LOG_TAG);
    let requirements = AreaStartup::request(parent.as_raw()).map_err(|_| Failed)?;
    let area = requirements.area.ok_or(Failed)?;
    let root = Str::new("/boot/scope-consumer-fixture").ok_or(Failed)?;
    let session = area
        .client()
        .with_wait(Timeout::from_secs(5))
        .resolve(root)
        .map_err(|_| Failed)?
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
