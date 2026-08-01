//! Процесс файловой системы в памяти: изменяемое дерево, живущее на бюджете этого процесса.
//! Байты образа ей не нужны, всё остальное делает общий цикл обслуживания.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

use core::panic::PanicInfo;

use filesystem::RamFileSystem;
use filesystem_server::serve;
use runtime::{log_fmt, thread_exit};
use syscall::Handle;

// Тег строк процесса в klog.
const LOG_TAG: &str = "filesystem-ramfs";

/// `admin` приходит в x0 как HandleId серверной стороны admin-порта.
#[unsafe(no_mangle)]
pub extern "C" fn _start(admin: Handle) -> ! {
    match serve(admin, LOG_TAG, |_| Ok(RamFileSystem::new())) {
        Ok(()) => thread_exit(0),
        Err(error) => {
            log_fmt(format_args!("stopped: {error}"));
            thread_exit(1)
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    thread_exit(1)
}
