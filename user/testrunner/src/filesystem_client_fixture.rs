#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use filesystem_abi::{AccessMode, OpenFlags, file_system};
use ipc::wire::Str;
use runtime::{PortTransport, thread_exit};
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
    let client = file_system::Client::new(PortTransport::client(service)).with_wait(CALL_TIMEOUT);

    // Выданный буфер закрывает ответ: фикстуре довольно самой выдачи.
    let Ok(response) = client.acquire_buffer(BUFFER_SIZE) else {
        return false;
    };
    if response.value.is_err() {
        return false;
    }

    let Some(path) = Str::new(PATH) else {
        return false;
    };
    let opened = client
        .open(path, AccessMode::ReadWrite, OpenFlags::CREATE)
        .map(|response| response.value);
    matches!(opened, Ok(Ok(_)))
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    thread_exit(1)
}
