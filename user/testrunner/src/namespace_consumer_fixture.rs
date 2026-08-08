#![no_std]
#![no_main]
#![allow(unsafe_code)]

use core::panic::PanicInfo;

use component_abi::component_manifest;
use filesystem_abi::{NamespaceClient, PATH_MAX};
use ipc::wire::Str;
use runtime::{OwnedHandle, PortTransport, Signal, Timeout, thread_exit};
use syscall::{Handle, SIGNALED};

const PROBE_PATH: &str = "/tmp";

const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const PROBE_TURN: Timeout = Timeout::from_millis(10);

const NO_STARTUP_EXIT_CODE: u64 = 1;
const RESOLVE_FAILED_EXIT_CODE: u64 = 3;
const PROVIDER_GONE_EXIT_CODE: u64 = 9;

component_manifest! {
    struct ConsumerStartup {
        slots: { namespace: service(protocol = "Namespace") },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(startup: Handle) -> ! {
    let startup = OwnedHandle::from_handle(startup);
    let Ok(Some(started)) = ConsumerStartup::receive(&PortTransport::server(startup.borrow()))
    else {
        thread_exit(NO_STARTUP_EXIT_CODE);
    };
    drop(startup);

    let session = OwnedHandle::adopt(started.namespace);
    let namespace = NamespaceClient::new(PortTransport::client(&session)).with_wait(CALL_TIMEOUT);
    let Some(path) = Str::<{ PATH_MAX }>::new(PROBE_PATH) else {
        thread_exit(RESOLVE_FAILED_EXIT_CODE);
    };
    if !resolve(&namespace, path) {
        thread_exit(RESOLVE_FAILED_EXIT_CODE);
    }

    // Провайдер разрешает путь, пока жив: его уход виден отказом транспорта.
    while resolve(&namespace, path) {
        pause(PROBE_TURN);
    }
    thread_exit(PROVIDER_GONE_EXIT_CODE)
}

fn resolve(namespace: &NamespaceClient<PortTransport>, path: Str<'_, { PATH_MAX }>) -> bool {
    match namespace.resolve(path) {
        Ok(Ok(mounted)) => {
            drop(OwnedHandle::adopt(mounted.session));
            true
        }
        _ => false,
    }
}

fn pause(timeout: Timeout) {
    let Ok(timer) = Signal::create() else {
        return;
    };
    let _ = timer.wait(SIGNALED, timeout);
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
