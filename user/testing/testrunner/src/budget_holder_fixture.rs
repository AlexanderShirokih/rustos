#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

extern crate alloc;

use alloc::boxed::Box;
use core::panic::PanicInfo;

use component_startup::component_manifest;
use filesystem_abi::{AccessMode, FileSystemError, OpenFlags, PATH_MAX, file_system};
use ipc::wire::Str;
use kernel_objects::{Port, Resource, SessionTransport};
use runtime::{MemoryAccess, Timeout, log_line, process_exit, start_component};
use syscall::{Handle, kind};
use userland::USERLAND_PAGE_SIZE;

mod probe_protocol;

use probe_protocol::{probe, serve_probe};

const LOG_TAG: &str = "holder";

const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const STARTED_PATH: &str = "/started";
const HELD_PATH: &str = "/held";

const FAILED_EXIT_CODE: u64 = 1;
const HELD_EXIT_CODE: u64 = 3;

component_manifest! {
    struct BudgetHolderStartup {
        requirements: { files: session(file_system::Protocol) },
        exports: [probe::Protocol],
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    start_component(parent, LOG_TAG);
    let Ok(requirements) = BudgetHolderStartup::request(parent) else {
        process_exit(FAILED_EXIT_CODE)
    };
    let files = requirements.files.client().with_wait(CALL_TIMEOUT);
    let (Some(started_path), Some(held_path)) = (Str::new(STARTED_PATH), Str::new(HELD_PATH))
    else {
        process_exit(FAILED_EXIT_CODE)
    };

    match files.open(started_path, AccessMode::Read, OpenFlags::NONE) {
        Ok(Ok(_)) => {
            let Ok(export) = Port::create() else {
                process_exit(FAILED_EXIT_CODE)
            };
            log_line("started");
            serve_probe(parent, Box::leak(Box::new(export)))
        }
        Ok(Err(FileSystemError::NotFound)) => {
            hold_budget(&files, started_path, held_path);
            log_line("budget held");
            process_exit(HELD_EXIT_CODE)
        }
        _ => process_exit(FAILED_EXIT_CODE),
    }
}

fn hold_budget(
    files: &file_system::Client<SessionTransport>,
    started: Str<'_, PATH_MAX>,
    held: Str<'_, PATH_MAX>,
) {
    if !matches!(
        files.open(started, AccessMode::ReadWrite, OpenFlags::CREATE),
        Ok(Ok(_))
    ) {
        process_exit(FAILED_EXIT_CODE)
    }
    let Ok(region) = Resource::self_resource()
        .and_then(|resource| resource.create_virtual(USERLAND_PAGE_SIZE, MemoryAccess::RW))
    else {
        process_exit(FAILED_EXIT_CODE)
    };
    if !matches!(files.create_backed(held, region, 0), Ok(Ok(_))) {
        process_exit(FAILED_EXIT_CODE)
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    process_exit(FAILED_EXIT_CODE)
}
