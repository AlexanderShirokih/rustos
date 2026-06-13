#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]
#![cfg_attr(target_os = "none", allow(unsafe_code))]

#[cfg(target_os = "none")]
use core::panic::PanicInfo;

#[cfg(target_os = "none")]
use bootstrap::{BootstrapClient, LOG_MESSAGE_MAX};
#[cfg(target_os = "none")]
use ipc::wire::Str;
#[cfg(target_os = "none")]
use syscall::CHANNEL_PEER_CLOSED;
#[cfg(target_os = "none")]
use runtime::{ChannelTransport, object_wait_one, thread_exit};

/// `bootstrap_handle` приходит в x0 как сырой HandleId WRITE-конца канала,
/// переданного ядром при спавне.
#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub extern "C" fn _start(bootstrap_handle: usize) -> ! {
    let client = BootstrapClient::new(ChannelTransport::new(bootstrap_handle));
    let _ = client
        .log(Str::<LOG_MESSAGE_MAX>::new("rootkeeper started").expect("startup log must fit"));

    let wait_ret = object_wait_one(bootstrap_handle, CHANNEL_PEER_CLOSED, u64::MAX);
    thread_exit(u64::from(wait_ret < 0))
}

#[cfg(target_os = "none")]
#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(not(target_os = "none"))]
fn main() {}
