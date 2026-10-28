#![no_std]
#![no_main]

extern crate alloc;

use alloc::rc::Rc;

use component_startup::{Requirements as _, SessionPort, component_manifest};
use executor::{Executor, PortChannel};
use filesystem_abi::{AccessMode, FileSystemError, OpenFlags, file_system, namespace};
use ipc::CallError;
use kernel_objects::{Resource, SessionTransport};
use runtime::{MemoryAccess, Timeout, log_line};
use syscall::{Handle, kind};
use userland::USERLAND_PAGE_SIZE;

mod failed;
mod probe_protocol;

use failed::Failed;
use probe_protocol::{probe, serve_probe};

const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const STARTED_PATH: &str = "/started";
const HELD_PATH: &str = "/held";

component_manifest! {
    struct BudgetHolderStartup {
        requirements: { files: session(namespace::Protocol) },
        exports: [probe::Protocol],
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Failed> {
    let executor = Rc::new(Executor::new().map_err(|_| Failed)?);

    executor.block_on(async {
        let files = BudgetHolderStartup::from_parent(parent)
            .files
            .open_async()
            .await
            .map_err(|_| Failed)?;
        let root = "/tmp";
        let namespace = PortChannel::client(files).with_wait(CALL_TIMEOUT);
        let session = namespace.resolve(root).await.map_err(|_| Failed)?;
        let files = PortChannel::client(kernel_objects::Session::from_session(session));
        let started_path = STARTED_PATH;
        let held_path = HELD_PATH;

        match files
            .open(started_path, AccessMode::Read, OpenFlags::NONE)
            .await
        {
            Ok(_) => {
                let export = SessionPort::create().map_err(|_| Failed)?;
                log_line("started");
                serve_probe(&executor, parent, export).await
            }
            Err(CallError::Remote(FileSystemError::NotFound)) => {
                hold_budget(&files, started_path, held_path).await?;
                log_line("budget held");
                Err(Failed)
            }
            _ => Err(Failed),
        }
    })
}

async fn hold_budget(
    files: &file_system::Client<PortChannel<SessionTransport>>,
    started: &str,
    held: &str,
) -> Result<(), Failed> {
    files
        .open(started, AccessMode::ReadWrite, OpenFlags::CREATE)
        .await
        .map_err(|_| Failed)?;
    let region = Resource::self_resource()
        .and_then(|resource| resource.create_virtual(USERLAND_PAGE_SIZE, MemoryAccess::RW))
        .map_err(|_| Failed)?;

    files
        .create_backed(held, region, 0)
        .await
        .map_err(|_| Failed)?;

    Ok(())
}
