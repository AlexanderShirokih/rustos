#![no_std]
#![no_main]
#![allow(unsafe_code)]

use core::panic::PanicInfo;

use bootstrap::{BootstrapClient, LOG_MESSAGE_MAX};
use ipc::wire::Str;
use runtime::{ChannelTransport, object_wait_one, thread_exit};
use syscall::CHANNEL_PEER_CLOSED;

/// `bootstrap_handle` приходит в x0 как сырой HandleId WRITE-конца канала,
/// переданного ядром при спавне.
#[unsafe(no_mangle)]
pub extern "C" fn _start(bootstrap_handle: usize) -> ! {
    let client = BootstrapClient::new(ChannelTransport::new(bootstrap_handle));
    let _ = client
        .log(Str::<LOG_MESSAGE_MAX>::new("rootkeeper started").expect("startup log must fit"));

    let wait_ret = object_wait_one(bootstrap_handle, CHANNEL_PEER_CLOSED, u64::MAX);
    thread_exit(u64::from(wait_ret < 0))
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
