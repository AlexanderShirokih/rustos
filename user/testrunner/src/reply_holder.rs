#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use kernel_objects::{OwnedHandle, Port};
use runtime::{Timeout, thread_exit};
use syscall::Handle;

/// Программа-фикстура: принимает запрос и уходит, не ответив на него.
/// `thread_exit` не разматывает стек, и reply-хэндл остаётся в таблице
/// процесса до её разрушения.
#[unsafe(no_mangle)]
pub extern "C" fn _start(service: Handle) -> ! {
    let port = Port::from_handle(OwnedHandle::from_handle(service));
    let received = port.recv(Timeout::INFINITE);
    thread_exit(u64::from(received.is_err()))
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
