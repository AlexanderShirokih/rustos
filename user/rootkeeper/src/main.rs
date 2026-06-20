#![no_std]
#![no_main]
#![allow(unsafe_code)]

use core::panic::PanicInfo;

use bootstrap::{BootstrapClient, LOG_MESSAGE_MAX};
use ipc::wire::Str;
use runtime::{PortTransport, thread_exit};
use syscall::Handle;

/// `bootstrap_handle` приходит в x0 как сырой HandleId bootstrap-port'а
/// (клиент-отправитель лога), переданного ядром при спавне.
#[unsafe(no_mangle)]
pub extern "C" fn _start(bootstrap_handle: usize) -> ! {
    let bootstrap = Handle::new(bootstrap_handle as u32).expect("bootstrap handle is non-zero");
    let client = BootstrapClient::new(PortTransport::client(bootstrap));
    let result = client
        .log(Str::<LOG_MESSAGE_MAX>::new("rootkeeper started").expect("startup log must fit"));

    // log - синхронный #[cast] (port_send блокирует до доставки); по его
    // завершении лог принят ядром. Port не сигналит peer-close, поэтому
    // просто выходим. Завершение процесса гасит машину (см. ядро init).
    thread_exit(u64::from(result.is_err()))
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
