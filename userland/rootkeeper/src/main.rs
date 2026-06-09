#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]
#![cfg_attr(target_os = "none", allow(unsafe_code))]

#[cfg(target_os = "none")]
use core::arch::asm;
#[cfg(target_os = "none")]
use core::panic::PanicInfo;

#[cfg(target_os = "none")]
use userland_abi::{
    BOOTSTRAP_ABI_VERSION, BOOTSTRAP_HEARTBEAT_PERIOD_NS, BOOTSTRAP_HEARTBEAT_SIZE,
    BOOTSTRAP_HELLO_MAGIC, BOOTSTRAP_HELLO_SIZE, CHANNEL_SIGNAL_PEER_CLOSED,
    SYSCALL_RETURN_TIMEOUT, SyscallOp, encode_bootstrap_heartbeat,
};

// svc-immediate обязан быть литералом; привязываем его к каноничному ABI-enum.
#[cfg(target_os = "none")]
const _: () = assert!(SyscallOp::ObjectWaitOne as u16 == 0x11);
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

    // Heartbeat-цикл: сон на bootstrap-канале до закрытия peer'а либо тика
    // периода. seq стартует с 0, шаг 1 (wrapping); break несёт exit code.
    let mut seq: u64 = 0;
    let exit_code: u64 = loop {
        let wait_ret: i64;
        // SAFETY: svc-immediate несёт номер операции (ESR.ISS), аргументы лежат
        // в x0..x2: handle, маска сигналов, timeout_ns; память ядру не
        // передаётся. x0 на выходе: observed-маска (>=0) либо -(SyscallError).
        unsafe {
            asm!(
                "svc #0x11", // SyscallOp::ObjectWaitOne = 0x11
                in("x0") bootstrap_handle as u64,
                in("x1") u64::from(CHANNEL_SIGNAL_PEER_CLOSED),
                in("x2") BOOTSTRAP_HEARTBEAT_PERIOD_NS,
                lateout("x0") wait_ret,
                options(nostack),
            );
        }

        // Маска ожидания - один бит PEER_CLOSED, значит любой неотрицательный
        // возврат означает закрытие kernel-конца: штатный выход.
        if wait_ret >= 0 {
            break 0;
        }
        // Точное сравнение с Timeout: только истёкший период считается тиком,
        // любая иная ошибка ведёт к немедленному выходу - горячий цикл
        // невозможен ни в одном ошибочном режиме.
        if wait_ret != SYSCALL_RETURN_TIMEOUT {
            break 1;
        }

        let frame = encode_bootstrap_heartbeat(seq);
        let write_ret: i64;
        // SAFETY: svc-immediate несёт номер операции (ESR.ISS), аргументы лежат
        // в x0..x4: handle, bytes_va, bytes_len, handles_va, handles_count.
        // Буфер `frame` живёт на стеке (замаплен user_rw), ядро читает его по
        // x1; отсутствие `nomem` не даёт переупорядочить запись буфера за svc.
        unsafe {
            asm!(
                "svc #0x21", // SyscallOp::ChannelWrite = 0x21
                in("x0") bootstrap_handle as u64,
                in("x1") frame.as_ptr() as u64,
                in("x2") BOOTSTRAP_HEARTBEAT_SIZE as u64,
                in("x3") 0_u64,
                in("x4") 0_u64,
                lateout("x0") write_ret,
                options(nostack),
            );
        }
        // Ошибка записи: peer закрылся в окне Timeout-write либо очередь полна
        // при мёртвом читателе - штатный выход.
        if write_ret < 0 {
            break 0;
        }
        seq = seq.wrapping_add(1);
    };

    // SAFETY: ThreadExit не возвращается, поэтому asm помечен noreturn и
    // удовлетворяет `-> !`; x0 несёт exit code.
    unsafe {
        asm!(
            "svc #0x52", // SyscallOp::ThreadExit = 0x52
            in("x0") exit_code,
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
