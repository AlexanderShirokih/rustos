#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_abi::{SESSION_SCOPE_MAX, SessionError};
use component_startup::component_manifest;
use ipc::{
    Transport as _,
    wire::{Cap, Str},
};
use kernel_objects::{OwnedHandle, Port, PortTransport, ReceivedFrame, kind};
use runtime::{Timeout, thread_exit};
use syscall::Handle;

const RECEIVED_EXIT_CODE: u64 = 1;
const NO_EARLIER_FRAME_EXIT_CODE: u64 = 2;
const NO_PARKING_EXIT_CODE: u64 = 3;

const EARLIER_FRAME_TIMEOUT: Timeout = Timeout::from_secs(5);

component_manifest! {
    struct OwnershipStartup {
        slots: { probe: service(protocol = "Ownership") },
    }
}

#[derive(Default)]
struct EarlierFrame(Option<Cap<kind::Any>>);

impl component_abi::component_startup::Service for EarlierFrame {
    fn begin(&mut self, log: Cap<kind::Port>, _export_count: u16, _slot_count: u16) {
        self.0 = Some(log.erase());
    }

    fn export(&mut self, capability: Cap<kind::Port>) {
        self.0 = Some(capability.erase());
    }

    fn slot(&mut self, capability: Cap<kind::Any>) {
        self.0 = Some(capability);
    }

    fn open_session(
        &mut self,
        _export: u16,
        _scope: Str<'_, SESSION_SCOPE_MAX>,
    ) -> Result<Cap<kind::Port>, SessionError> {
        Err(SessionError::Refused)
    }
}

/// Программа-фикстура старого содержимого IPC-буфера. Она принимает свой кадр до стартовой
/// транзакции и держит его capability, пока приём транзакции отказывает. Процесс паркуется,
/// оставляя таблицу handle живой для наблюдения вызывающим.
/// `startup` - серверная сторона startup-порта, аргумент точки входа.
#[unsafe(no_mangle)]
pub extern "C" fn _start(startup: Handle<kind::Port>) -> ! {
    let startup = Port::from_handle(startup);
    let Some(kept) = earlier_capability(&startup) else {
        thread_exit(NO_EARLIER_FRAME_EXIT_CODE);
    };

    if OwnershipStartup::receive(startup).is_ok() {
        thread_exit(RECEIVED_EXIT_CODE);
    }

    // Парковка без кадров держит принятую capability живой. У свежего порта нет
    // отправителей, и его приём не вернётся.
    let Ok(parking) = Port::create() else {
        thread_exit(NO_PARKING_EXIT_CODE);
    };
    let _ = parking.recv(Timeout::INFINITE);
    drop(kept);
    thread_exit(NO_PARKING_EXIT_CODE)
}

fn earlier_capability(startup: &Port) -> Option<OwnedHandle<kind::Any>> {
    let transport = PortTransport::server(startup);
    transport.wait_readable(EARLIER_FRAME_TIMEOUT).ok()?;
    let mut received = ReceivedFrame::take(&transport)?;
    let mut frame = EarlierFrame::default();
    let _ = component_abi::component_startup::dispatch(&mut frame, &transport);
    received.claim(frame.0?)
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
