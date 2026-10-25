#![no_std]
#![no_main]

use component_startup::{Requirements as _, component_manifest};
use filesystem_abi::{AccessMode, FileSystemError, NodeKind, OpenFlags, file_system, namespace};
use ipc::CallError;
use kernel_objects::SessionTransport;
use runtime::{Timeout, log_line};
use syscall::{Handle, kind};

mod failed;
#[allow(dead_code)]
mod filesystem_io;

use failed::Failed;
use filesystem_io::IoClient;

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
        .wait()
        .map_err(|_| Failed)?;
    let area = IoClient::new(kernel_objects::Session::from_session(session).client());

    if inspect(&area) {
        log_line("area read");
    }

    Err(Failed)
}

fn inspect(client: &IoClient) -> bool {
    let Ok(opened) = client
        .open(RECORD, AccessMode::Read, OpenFlags::NONE)
        .wait()
    else {
        return false;
    };

    if opened.info.kind != NodeKind::File || opened.info.size != RECORD_BODY.len() as u64 {
        return false;
    }

    let mut body = [0; RECORD_BODY.len()];
    let Ok(count) = client.read(opened.fd, 0, &mut body) else {
        return false;
    };

    if count as usize != RECORD_BODY.len() {
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
    client.open(text, AccessMode::Read, OpenFlags::NONE).wait() != Err(CallError::Remote(refused))
}
