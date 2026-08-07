#![no_std]
#![no_main]
#![allow(unsafe_code)]

use core::panic::PanicInfo;

use filesystem_abi::{FileSystemAdminService, FileSystemError, dispatch_file_system_admin};
use ipc::{Transport as _, wire::Cap};
use runtime::{OwnedHandle, Port, PortTransport, thread_exit};
use syscall::{Handle, Timeout};

struct SessionlessAdmin;

impl FileSystemAdminService for SessionlessAdmin {
    fn start(&mut self, log: Cap, image: Cap) {
        drop(OwnedHandle::adopt(log));
        drop(OwnedHandle::adopt(image));
    }

    fn bind(&mut self) -> Result<Cap, FileSystemError> {
        Err(FileSystemError::NoSession)
    }
}

/// Программа-фикстура зависшего монтирования: принимает стартовый кадр и замолкает. Admin-порт
/// остаётся живым, но кадры по нему больше не читаются, и встречный `call` ждёт до своего срока.
/// `admin` - серверная сторона admin-порта.
#[unsafe(no_mangle)]
pub extern "C" fn _start(admin: Handle) -> ! {
    let admin = OwnedHandle::from_handle(admin);
    let transport = PortTransport::server(admin.borrow());
    let _ = transport.wait_readable(Timeout::INFINITE);
    let _ = dispatch_file_system_admin(&mut SessionlessAdmin, &transport);

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
