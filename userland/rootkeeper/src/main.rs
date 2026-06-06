#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]
#![cfg_attr(target_os = "none", allow(unsafe_code))]

#[cfg(target_os = "none")]
use core::arch::asm;
#[cfg(target_os = "none")]
use core::panic::PanicInfo;

#[cfg(target_os = "none")]
use userland_abi::{BOOTSTRAP_ABI_VERSION, BOOTSTRAP_HELLO_MAGIC, BOOTSTRAP_HELLO_SIZE, SyscallOp};

// svc-immediate обязан быть литералом; привязываем его к каноничному ABI-enum.
#[cfg(target_os = "none")]
const _: () = assert!(SyscallOp::ChannelWrite as u16 == 0x21);
#[cfg(target_os = "none")]
const _: () = assert!(SyscallOp::ThreadExit as u16 == 0x52);

/// `bootstrap_handle` приходит в x0 как сырой HandleId WRITE-конца канала,
/// переданного ядром при спавне.
#[cfg(target_os = "none")]
#[unsafe(no_mangle)]
pub extern "C" fn _start(bootstrap_handle: usize) -> ! {
    let mut hello = [0u8; BOOTSTRAP_HELLO_SIZE];
    hello[..8].copy_from_slice(&BOOTSTRAP_HELLO_MAGIC);
    hello[8..].copy_from_slice(&BOOTSTRAP_ABI_VERSION.to_le_bytes());

    // SAFETY: svc-immediate несёт номер операции (ESR.ISS), аргументы лежат
    // в x0..x4: handle, bytes_va, bytes_len, handles_va, handles_count.
    // Буфер `hello` живёт на стеке (замаплен user_rw), ядро читает его по x1;
    // отсутствие `nomem` не даёт переупорядочить запись буфера за svc.
    unsafe {
        asm!(
            "svc #0x21", // SyscallOp::ChannelWrite = 0x21
            in("x0") bootstrap_handle as u64,
            in("x1") hello.as_ptr() as u64,
            in("x2") BOOTSTRAP_HELLO_SIZE as u64,
            in("x3") 0_u64,
            in("x4") 0_u64,
            lateout("x0") _,
            options(nostack),
        );
    }

    // SAFETY: ThreadExit не возвращается, поэтому asm помечен noreturn и
    // удовлетворяет `-> !`; x0 несёт exit code.
    unsafe {
        asm!(
            "svc #0x52", // SyscallOp::ThreadExit = 0x52
            in("x0") 0_u64,
            options(noreturn, nostack),
        );
    }
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
