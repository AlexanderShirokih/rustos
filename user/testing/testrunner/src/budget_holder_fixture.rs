#![no_std]
#![no_main]
#![allow(unsafe_code)]

extern crate alloc;

use alloc::boxed::Box;

use component_startup::{Requirements as _, component_manifest};
use filesystem_abi::{AccessMode, FileSystemError, OpenFlags, PATH_MAX, file_system, namespace};
use ipc::wire::Str;
use kernel_objects::{Port, Resource, SessionTransport};
use runtime::{MemoryAccess, Timeout, log_line, start_component};
use userland::USERLAND_PAGE_SIZE;

mod fixture_start;
mod probe_protocol;

use fixture_start::{Failed, fixture_start};
use probe_protocol::{probe, serve_probe};

const LOG_TAG: &str = "holder";

const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const STARTED_PATH: &str = "/started";
const HELD_PATH: &str = "/held";

component_manifest! {
    struct BudgetHolderStartup {
        requirements: { files: session(namespace::Protocol) },
        exports: [probe::Protocol],
    }
}

fixture_start!(run);

fn run(parent: &Port) -> Result<(), Failed> {
    start_component(parent.as_raw(), LOG_TAG);
    let requirements = BudgetHolderStartup::request(parent.as_raw()).map_err(|_| Failed)?;

    let files = requirements.files.ok_or(Failed)?;
    let root = Str::new("/tmp").ok_or(Failed)?;
    let session = files
        .client()
        .with_wait(CALL_TIMEOUT)
        .resolve(root)
        .map_err(|_| Failed)?
        .map_err(|_| Failed)?;
    let files = kernel_objects::Session::from_session(session)
        .client()
        .with_wait(CALL_TIMEOUT);
    let started_path = Str::new(STARTED_PATH).ok_or(Failed)?;
    let held_path = Str::new(HELD_PATH).ok_or(Failed)?;

    match files.open(started_path, AccessMode::Read, OpenFlags::NONE) {
        Ok(Ok(_)) => {
            let export = Port::create().map_err(|_| Failed)?;
            log_line("started");
            serve_probe(parent.as_raw(), Box::leak(Box::new(export)))
        }
        Ok(Err(FileSystemError::NotFound)) => {
            hold_budget(&files, started_path, held_path)?;
            log_line("budget held");
            Err(Failed)
        }
        _ => Err(Failed),
    }
}

fn hold_budget(
    files: &file_system::Client<SessionTransport>,
    started: Str<'_, PATH_MAX>,
    held: Str<'_, PATH_MAX>,
) -> Result<(), Failed> {
    files
        .open(started, AccessMode::ReadWrite, OpenFlags::CREATE)
        .map_err(|_| Failed)?
        .map_err(|_| Failed)?;
    let region = Resource::self_resource()
        .and_then(|resource| resource.create_virtual(USERLAND_PAGE_SIZE, MemoryAccess::RW))
        .map_err(|_| Failed)?;
    files
        .create_backed(held, region, 0)
        .map_err(|_| Failed)?
        .map_err(|_| Failed)?;
    Ok(())
}
