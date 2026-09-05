#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_abi::component_startup;
use component_client::component_manifest;
use ipc::{Transport as _, wire::Cap};
use kernel_objects::{OwnedHandle, Port, PortTransport, ReceivedFrame};
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
struct EarlierFrame(Option<Cap>);

impl component_startup::Service for EarlierFrame {
    fn start(&mut self, log: Cap, _control: Cap, _service: Cap, _slot_count: u16) {
        self.0 = Some(log);
    }

    fn slot(&mut self, capability: Cap) {
        self.0 = Some(capability);
    }
}

/// Программа-фикстура старого содержимого IPC-буфера. Она принимает свой кадр до стартовой
/// транзакции и держит его capability, пока приём транзакции отказывает. Процесс паркуется,
/// оставляя таблицу handle живой для наблюдения вызывающим. `startup` приходит в x0 как
/// HandleId одноразового startup-порта.
#[unsafe(no_mangle)]
pub extern "C" fn _start(startup: Handle) -> ! {
    let startup = OwnedHandle::from_handle(startup);
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

fn earlier_capability(startup: &OwnedHandle) -> Option<OwnedHandle> {
    let transport = PortTransport::server(startup);
    transport.wait_readable(EARLIER_FRAME_TIMEOUT).ok()?;
    let mut received = ReceivedFrame::take(&transport)?;
    let mut frame = EarlierFrame::default();
    let _ = component_startup::dispatch(&mut frame, &transport);
    received.claim(frame.0?)
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
