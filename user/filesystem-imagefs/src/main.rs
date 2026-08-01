//! Процесс файловой системы поверх userland-образа: read-only дерево, чьи узлы лежат байтами
//! региона образа. Содержимое узла выдаётся окном этого региона, без копирования.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

use core::panic::PanicInfo;

use filesystem::ImageFileSystem;
use filesystem_server::serve;
use runtime::{log_fmt, thread_exit};
use syscall::Handle;

// Тег строк процесса в klog.
const LOG_TAG: &str = "filesystem-imagefs";

/// `admin` приходит в x0 как HandleId серверной стороны admin-порта.
#[unsafe(no_mangle)]
pub extern "C" fn _start(admin: Handle) -> ! {
    match serve(admin, LOG_TAG, |image| Ok(ImageFileSystem::new(image)?)) {
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
