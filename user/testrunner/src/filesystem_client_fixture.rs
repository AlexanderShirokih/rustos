#![no_std]
#![no_main]
#![allow(unsafe_code)]

use core::panic::PanicInfo;

use filesystem_abi::{AccessMode, FileSystemClient, OpenFlags};
use ipc::wire::Str;
use runtime::{OwnedHandle, PortTransport, thread_exit};
use syscall::{Handle, Timeout};

const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const BUFFER_SIZE: u32 = 64 * 1024;

const PATH: &str = "/fixture";

/// Программа-клиент для теста уборки сессий: берёт буфер, открывает узел и
/// завершается. Её капа сервисного порта умирает вместе с процессом, и сервер
/// узнаёт об уходе значка сессии. `service` - заклеймённая капа сервисного
/// порта.
#[unsafe(no_mangle)]
pub extern "C" fn _start(service: Handle) -> ! {
    thread_exit(u64::from(!populate_session(service)))
}

fn populate_session(service: Handle) -> bool {
    let client = FileSystemClient::new(PortTransport::client(service)).with_wait(CALL_TIMEOUT);

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
