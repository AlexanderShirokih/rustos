#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use kernel_objects::{Port, Signal, kind};
use runtime::{Timeout, handle_close, thread_exit};
use syscall::Handle;

const NO_PARKING_EXIT_CODE: u64 = 1;
const EMPTY_TABLE_EXIT_CODE: u64 = 2;
const RECEIVED_EXIT_CODE: u64 = 3;

/// Программа-фикстура заполненной таблицы handle. Она оставляет ровно один свободный слот,
/// его занимает capability доставленного кадра, и для reply-объекта вызова места уже не
/// остаётся. Процесс паркуется, оставляя таблицу живой для наблюдения вызывающим.
/// `startup` - серверная сторона startup-порта, аргумент точки входа.
#[unsafe(no_mangle)]
pub extern "C" fn _start(startup: Handle<kind::Port>) -> ! {
    let port = Port::from_handle(startup);
    // Порт парковки создаётся до заполнения таблицы, потому что свободных слотов после
    // него не останется.
    let Ok(parking) = Port::create() else {
        thread_exit(NO_PARKING_EXIT_CODE);
    };

    let Some(spare) = fill_handle_table() else {
        thread_exit(EMPTY_TABLE_EXIT_CODE);
    };
    // Освобождённый слот принимает capability кадра, и для reply-объекта слота не остаётся.
    let _ = handle_close(spare);

    if port.recv(Timeout::INFINITE).is_ok() {
        thread_exit(RECEIVED_EXIT_CODE);
    }

    // Парковка без кадров. У свежего порта нет отправителей, и его приём не вернётся.
    let _ = parking.recv(Timeout::INFINITE);
    thread_exit(NO_PARKING_EXIT_CODE)
}

fn fill_handle_table() -> Option<Handle<kind::Signal>> {
    let mut last = None;
    while let Ok(signal) = Signal::create() {
        last = Some(signal.into_raw());
    }
    last
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
