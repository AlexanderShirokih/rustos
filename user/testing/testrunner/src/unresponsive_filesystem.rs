#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_abi::component_startup as startup_contract;
use component_startup::{component_manifest, session};
use filesystem_abi::{file_system, namespace_admin};
use ipc::wire::IpcError;
use kernel_objects::{Port, Session};
use runtime::{PortTransport, process_exit};
use syscall::{Handle, Rights, Timeout, kind};

const NAMESPACE_REQUIREMENT: &str = "namespace";

const FAILED_EXIT_CODE: u64 = 1;

component_manifest! {
    struct UnresponsiveFileSystemStartup {
        requirements: { namespace: session(namespace_admin::Protocol) },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    let Ok(started) = UnresponsiveFileSystemStartup::start(parent) else {
        process_exit(FAILED_EXIT_CODE)
    };
    drop(started.log);
    let Ok(service) = Port::create() else {
        process_exit(FAILED_EXIT_CODE)
    };

    let client = startup_contract::Client::new(PortTransport::new(parent));
    let mut admin = started.requirements.namespace;
    loop {
        let Ok(copy) = service.duplicate(Rights::WRITE | Rights::TRANSFER, None) else {
            process_exit(FAILED_EXIT_CODE)
        };
        let filesystem = Session::<file_system::Protocol>::from_handle(copy);
        if !matches!(admin.client().mount(filesystem), Err(IpcError::PeerClosed)) {
            break;
        }
        match session::<namespace_admin::Protocol>(&client, NAMESPACE_REQUIREMENT) {
            Ok(Ok(renewed)) => admin = renewed,
            _ => break,
        }
    }

    let Ok(parking) = Port::create() else {
        process_exit(FAILED_EXIT_CODE)
    };
    let _ = parking.recv(Timeout::INFINITE);
    process_exit(FAILED_EXIT_CODE)
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    process_exit(FAILED_EXIT_CODE)
}
