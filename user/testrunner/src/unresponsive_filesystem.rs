#![no_std]
#![no_main]
#![allow(unsafe_code)]

use core::panic::PanicInfo;

use component_abi::component_manifest;
use filesystem_abi::{FileSystemError, file_system_admin};
use ipc::{Transport as _, wire::Cap};
use runtime::{OwnedHandle, Port, PortTransport, thread_exit};
use syscall::{Handle, Rights, Timeout};

struct SessionlessAdmin {
    service: OwnedHandle,
    vended: Option<OwnedHandle>,
}

component_manifest! {
    struct UnresponsiveFileSystemStartup {
        slots: {},
    }
}

impl file_system_admin::Service for SessionlessAdmin {
    fn bind(&mut self) -> Result<Cap, FileSystemError> {
        let session = self
            .service
            .duplicate(Rights::WRITE | Rights::TRANSFER, None)
            .map_err(|_| FileSystemError::NoSpace)?;
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
    let delivered = file_system_admin::dispatch(&mut admin, &transport).is_ok();
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
