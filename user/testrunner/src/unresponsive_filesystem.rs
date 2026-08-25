#![no_std]
#![no_main]
#![allow(unsafe_code)]

use core::panic::PanicInfo;

use component_abi::{SESSION_SCOPE_MAX, SessionError, component_manifest, component_sessions};
use ipc::{
    Transport as _,
    wire::{Cap, Str},
};
use runtime::{OwnedHandle, Port, PortTransport, thread_exit};
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

/// Программа-фикстура зависшей файловой системы: принимает стартовую транзакцию и замолкает.
#[unsafe(no_mangle)]
pub extern "C" fn _start(startup: Handle) -> ! {
    let startup = OwnedHandle::from_handle(startup);
    let Ok(Some(started)) =
        UnresponsiveFileSystemStartup::receive(&PortTransport::server(startup.borrow()))
    else {
        thread_exit(1)
    };
    drop(OwnedHandle::adopt(started.log));
    let control = OwnedHandle::adopt(started.control);
    let service = OwnedHandle::adopt(started.service);
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

    // Парковка без кадров: у свежего порта нет отправителей, его приём не вернётся никогда.
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
