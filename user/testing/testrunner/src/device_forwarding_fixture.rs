#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

extern crate alloc;

use alloc::boxed::Box;
use core::panic::PanicInfo;

use component_abi::component_startup as startup_contract;
use component_startup::{component_manifest, serve_export, whole_export};
use ipc::{IntoWireValue, Listener as _, ReceivedResources, RecvError, wire::MESSAGE_INLINE_MAX};
use kernel_objects::{IrqLine, MemoryRegion, Port};
use runtime::{PortTransport, Priority, Timeout, process_exit, spawn_owned};
use syscall::{Handle, Rights, kind};

mod device_forwarding_protocol;

use device_forwarding_protocol::{FORWARDED_EXIT_CODE, Forwarded, device_forwarding};

const EXPORT_STACK: u64 = 0x4000;

const FAILED_EXIT_CODE: u64 = 1;
const EXPORT_STOPPED_EXIT_CODE: u64 = 5;

component_manifest! {
    struct ForwardingStartup {
        requirements: {
            line: irq(compatible = ["arm,pl031"]),
            window: mmio(compatible = ["arm,pl031"]),
        },
        exports: [device_forwarding::Protocol],
    }
}

struct Forwarder {
    copies: Option<(IrqLine, MemoryRegion)>,
}

impl<F: ReceivedResources> device_forwarding::Service<F> for Forwarder {
    fn forward(&mut self, _frame: &F) -> impl IntoWireValue<Read = Forwarded> {
        let Some((line, window)) = self.copies.take() else {
            process_exit(FAILED_EXIT_CODE)
        };
        Forwarded { line, window }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    let Ok(started) = ForwardingStartup::start(parent) else {
        process_exit(FAILED_EXIT_CODE)
    };
    drop(started.log);
    let rights = Rights::READ | Rights::WRITE | Rights::TRANSFER;
    let (Ok(line), Ok(window)) = (
        started.requirements.line.duplicate(rights, None),
        started.requirements.window.duplicate(rights, None),
    ) else {
        process_exit(FAILED_EXIT_CODE)
    };
    let Ok(export) = Port::create() else {
        process_exit(FAILED_EXIT_CODE)
    };
    let export: &'static Port = Box::leak(Box::new(export));

    let exporting = spawn_owned(
        move || {
            let parent = startup_contract::Client::new(PortTransport::new(parent));
            let _ = serve_export::<device_forwarding::Protocol>(&parent, whole_export(export));
            process_exit(EXPORT_STOPPED_EXIT_CODE)
        },
        EXPORT_STACK,
        Priority::new(1),
    );
    if exporting.is_err() {
        process_exit(FAILED_EXIT_CODE)
    }

    let mut forwarder = Forwarder {
        copies: Some((line, window)),
    };
    let transport = PortTransport::new(export.as_raw());
    let mut bytes = [0u8; MESSAGE_INLINE_MAX];
    loop {
        match transport.recv(&mut bytes, Timeout::INFINITE) {
            Ok(frame) => {
                let _ = device_forwarding::dispatch(&mut forwarder, frame);
                if forwarder.copies.is_none() {
                    process_exit(FORWARDED_EXIT_CODE)
                }
            }
            Err(RecvError::SenderGone(_) | RecvError::Discarded | RecvError::Timeout) => {}
            Err(RecvError::Closed) => process_exit(EXPORT_STOPPED_EXIT_CODE),
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    process_exit(FAILED_EXIT_CODE)
}
