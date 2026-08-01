#![no_std]
#![no_main]
#![allow(unsafe_code)]

use core::panic::PanicInfo;

use filesystem_abi::{AccessMode, FileSystemClient, OpenFlags};
use ipc::wire::Str;
use runtime::{OwnedHandle, PortTransport, thread_exit};
use syscall::Handle;

// Конечный срок ответа сервера превращает зависание фикстуры в диагностируемую ошибку.
const CALL_TIMEOUT_NS: u64 = 2_000_000_000;

// Буфер в потолок: сессия заметно весит на бюджете монтирования, пока сервер её не убрал.
const BUFFER_SIZE: u32 = 64 * 1024;

// Узел, который фикстура открывает: его дескриптор - первый в сессии.
const PATH: &str = "/fixture";

/// Программа-клиент для теста уборки сессий: берёт буфер, открывает узел и
/// завершается. Её капа сервисного порта умирает вместе с процессом, и сервер
/// узнаёт об уходе значка сессии. `service` - заклеймённая капа сервисного
/// порта.
#[unsafe(no_mangle)]
pub extern "C" fn _start(service: Handle) -> ! {
    thread_exit(u64::from(!populate_session(service)))
}

// Буфер данных и открытый дескриптор остаются за сессией до уборки.
fn populate_session(service: Handle) -> bool {
    let client =
        FileSystemClient::new(PortTransport::client(service)).with_wait_ns(CALL_TIMEOUT_NS);

    let Ok(Ok(buffer)) = client.acquire_buffer(BUFFER_SIZE) else {
        return false;
    };
    drop(OwnedHandle::adopt(buffer));

    let Some(path) = Str::new(PATH) else {
        return false;
    };
    let opened = client.open(path, AccessMode::ReadWrite, OpenFlags::CREATE);
    matches!(opened, Ok(Ok(_)))
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    thread_exit(1)
}
