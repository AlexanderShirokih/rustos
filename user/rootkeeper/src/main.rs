#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]
#![cfg_attr(target_os = "none", allow(unsafe_code))]

#[cfg(target_os = "none")]
use core::panic::PanicInfo;

#[cfg(target_os = "none")]
use userland_abi::{
    BOOTSTRAP_ABI_VERSION, BOOTSTRAP_HELLO_MAGIC, BOOTSTRAP_HELLO_SIZE, CHANNEL_SIGNAL_PEER_CLOSED,
};
#[cfg(target_os = "none")]
use userland_rt::{channel_write, object_wait_one, thread_exit};

/// `bootstrap_handle` приходит в x0 как сырой HandleId WRITE-конца канала,
/// переданного ядром при спавне.
#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub extern "C" fn _start(bootstrap_handle: usize) -> ! {
    let mut hello = [0u8; BOOTSTRAP_HELLO_SIZE];
    hello[..8].copy_from_slice(&BOOTSTRAP_HELLO_MAGIC);
    hello[8..].copy_from_slice(&BOOTSTRAP_ABI_VERSION.to_le_bytes());
    let _ = channel_write(bootstrap_handle, &hello);

    let wait_ret = object_wait_one(bootstrap_handle, CHANNEL_SIGNAL_PEER_CLOSED, u64::MAX);
    thread_exit(if wait_ret >= 0 { 0 } else { 1 })
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
