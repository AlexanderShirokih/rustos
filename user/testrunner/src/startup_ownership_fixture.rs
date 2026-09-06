#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_startup::component_manifest;
use kernel_objects::{OwnedHandle, Port};
use runtime::{Timeout, thread_exit};
use syscall::Handle;

const RECEIVED_EXIT_CODE: u64 = 1;
const NO_PARKING_EXIT_CODE: u64 = 2;

component_manifest! {
    struct OwnershipStartup {
        slots: { probe: service(protocol = "Ownership") },
    }
}

/// Программа-фикстура владения стартовым набором. Приём, не сложившийся в контракт, закрывает
/// уже доставленные capability, и процесс паркуется, оставляя таблицу handle живой для
/// наблюдения вызывающим. `startup` приходит в x0 как HandleId одноразового startup-порта.
#[unsafe(no_mangle)]
pub extern "C" fn _start(startup: Handle) -> ! {
    if OwnershipStartup::receive(OwnedHandle::from_handle(startup)).is_ok() {
        thread_exit(RECEIVED_EXIT_CODE);
    }

    // Парковка без кадров. У свежего порта нет отправителей, и его приём не вернётся.
    let Ok(parking) = Port::create() else {
        thread_exit(NO_PARKING_EXIT_CODE);
    };
    let _ = parking.recv(Timeout::INFINITE);
    thread_exit(NO_PARKING_EXIT_CODE)
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
