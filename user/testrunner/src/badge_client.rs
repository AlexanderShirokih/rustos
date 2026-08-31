#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use kernel_objects::{OwnedHandle, Port};
use runtime::{Timeout, thread_exit};
use syscall::Handle;

/// Программа-фикстура: шлёт один кадр и уходит, не закрывая клиентскую капу.
/// `thread_exit` не разматывает стек, и капа остаётся в таблице процесса
/// до её разрушения.
#[unsafe(no_mangle)]
pub extern "C" fn _start(client: Handle) -> ! {
    let port = Port::from_handle(OwnedHandle::from_handle(client));
    let sent = port.send_bytes(b"badged", Timeout::INFINITE);
    thread_exit(u64::from(sent.is_err()))
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
