#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_abi::{SESSION_SCOPE_MAX, SessionError, component_sessions};
use component_startup::{ReceivedStartup, component_manifest};
use ipc::{
    Transport as _,
    wire::{Cap, Str},
};
use kernel_objects::{OwnedHandle, Port};
use runtime::{PortTransport, thread_exit};
use syscall::{Handle, Rights, Timeout};

struct SessionlessAdmin {
    service: OwnedHandle,
    vended: Option<OwnedHandle>,
}

component_manifest! {
    struct UnresponsiveFileSystemStartup {
        exports: "FileSystem",
        establishes_sessions: true,
        slots: {},
    }
}

impl component_sessions::Service for SessionlessAdmin {
    fn open_session(&mut self, _scope: Str<'_, SESSION_SCOPE_MAX>) -> Result<Cap, SessionError> {
        let session = self
            .service
            .duplicate(Rights::WRITE | Rights::TRANSFER, None)
            .map_err(|_| SessionError::NoSpace)?;
        let cap = Cap::from_raw(session.as_raw().as_nonzero());
        self.vended = Some(session);
        Ok(cap)
    }
}

/// Программа-фикстура зависшей файловой системы принимает стартовую транзакцию и
/// перестаёт отвечать.
#[unsafe(no_mangle)]
pub extern "C" fn _start(startup: Handle) -> ! {
    let Ok(ReceivedStartup { log, resources }) =
        UnresponsiveFileSystemStartup::receive(OwnedHandle::from_handle(startup))
    else {
        thread_exit(1)
    };
    drop(log);
    let UnresponsiveFileSystemStartup { control, service } = resources;
    let transport = PortTransport::server(control.borrow());
    let _ = transport.wait_readable(Timeout::INFINITE);
    let mut admin = SessionlessAdmin {
        service,
        vended: None,
    };
    let delivered = component_sessions::dispatch(&mut admin, &transport).is_ok();
    if let Some(session) = admin.vended.take() {
        if delivered {
            let _ = session.into_raw();
        } else {
            let _ = session.close();
        }
    }

    // Парковка идёт без кадров. У свежего порта нет отправителей, и приём не вернётся никогда.
    let Ok(parking) = Port::create() else {
        thread_exit(1)
    };
    let _ = parking.recv(Timeout::INFINITE);
    thread_exit(0)
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    thread_exit(1)
}
