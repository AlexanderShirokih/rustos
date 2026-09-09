#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_abi::{SESSION_SCOPE_MAX, SessionError};
use component_startup::{ReceivedStartup, component_manifest};
use ipc::{
    Transport as _,
    wire::{Cap, Str},
};
use kernel_objects::{Port, kind};
use runtime::{PortTransport, thread_exit};
use syscall::{Handle, Rights, Timeout};

struct SessionlessAdmin {
    service: Port,
    vended: Option<Port>,
}

component_manifest! {
    struct UnresponsiveFileSystemStartup {
        exports: { service: "FileSystem" },
        establishes_sessions: true,
        slots: {},
    }
}

impl component_abi::component_startup::Service for SessionlessAdmin {
    fn begin(&mut self, _log: Cap<kind::Port>, _export_count: u16, _slot_count: u16) {}

    fn export(&mut self, _capability: Cap<kind::Port>) {}

    fn slot(&mut self, _capability: Cap<kind::Any>) {}

    fn open_session(
        &mut self,
        _export: u16,
        _scope: Str<'_, SESSION_SCOPE_MAX>,
    ) -> Result<Cap<kind::Port>, SessionError> {
        let session = self
            .service
            .duplicate(Rights::WRITE | Rights::TRANSFER, None)
            .map_err(|_| SessionError::NoSpace)?;
        let cap = Cap::from_handle(session.as_raw());
        self.vended = Some(session);
        Ok(cap)
    }
}

/// Программа-фикстура зависшей файловой системы принимает стартовую транзакцию и
/// перестаёт отвечать.
#[unsafe(no_mangle)]
pub extern "C" fn _start(startup: Handle<kind::Port>) -> ! {
    let Ok(ReceivedStartup { log, resources }) =
        UnresponsiveFileSystemStartup::receive(Port::from_handle(startup))
    else {
        thread_exit(1)
    };
    drop(log);
    let UnresponsiveFileSystemStartup { startup, service } = resources;
    let transport = PortTransport::server(startup.borrow());
    let _ = transport.wait_readable(Timeout::INFINITE);
    let mut admin = SessionlessAdmin {
        service,
        vended: None,
    };
    let delivered = component_abi::component_startup::dispatch(&mut admin, &transport).is_ok();
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
