#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_startup::{Requirements as _, component_manifest, serve_export, whole_export};
use filesystem_abi::file_system;
use kernel_objects::Port;
use runtime::process_exit;
use syscall::{Handle, kind};

const FAILED_EXIT_CODE: u64 = 1;

component_manifest! {
    struct UnresponsiveFileSystemStartup {
        requirements: {},
        exports: [file_system::Protocol],
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    let Ok(requirements) = UnresponsiveFileSystemStartup::request(parent) else {
        process_exit(FAILED_EXIT_CODE)
    };
    let Ok(service) = Port::create() else {
        process_exit(FAILED_EXIT_CODE)
    };
    let _ = serve_export::<file_system::Protocol>(&requirements.parent, whole_export(&service));
    process_exit(FAILED_EXIT_CODE)
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    process_exit(FAILED_EXIT_CODE)
}
