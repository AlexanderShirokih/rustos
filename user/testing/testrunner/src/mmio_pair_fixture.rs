#![no_std]
#![no_main]
#![allow(unsafe_code)]
#![allow(clippy::expect_used)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_startup::component_manifest;
use io::mmio::Reg;
use runtime::{log_fmt, process_exit, start_component};
use syscall::{Handle, kind};

const LOG_TAG: &str = "mmio-pair";

const MAGIC_VALUE: Reg<u32> = Reg::new(0x000);
const VIRTIO_MMIO_MAGIC_VALUE: u32 = 0x7472_6976;

component_manifest! {
    struct MmioPairStartup {
        requirements: {
            first: mmio(compatible = ["virtio,mmio"]),
            second: mmio(compatible = ["virtio,mmio"]),
        },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    start_component(parent, LOG_TAG);

    let requirements = MmioPairStartup::request(parent).expect("mmio requirements");

    let first = requirements.first.region.inspect().expect("first region");
    let second = requirements.second.region.inspect().expect("second region");

    let mapping = requirements.second.map().expect("second window mapping");
    assert_eq!(
        mapping.mmio().read_reg(MAGIC_VALUE),
        VIRTIO_MMIO_MAGIC_VALUE,
        "second window magic value"
    );

    log_fmt(format_args!(
        "regions {:#x} {:#x} offsets {:#x} {:#x}",
        first.base, second.base, requirements.first.offset, requirements.second.offset
    ));
    process_exit(0)
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
