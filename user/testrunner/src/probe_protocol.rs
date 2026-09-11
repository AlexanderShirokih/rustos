//! Экспорт `Probe` фикстур композиции.

// Модуль подключён к прогону и к фикстурам, и каждый бинарь пользуется своим подмножеством.
#![allow(dead_code)]

use component_abi::component_startup as startup_contract;
use component_startup::{serve_export, whole_export};
use ipc::{Listener as _, ReceivedResources, RecvError, wire::MESSAGE_INLINE_MAX};
use kernel_objects::Port;
use runtime::{PortTransport, Priority, Timeout, process_exit, spawn_owned};
use syscall::{Handle, kind};

const EXPORT_STOPPED_EXIT_CODE: u64 = 6;

const EXPORT_STACK: u64 = 0x4000;

ipc::protocol! {
    pub mod probe {
        name = "Probe";

        call fn ping(value: u64) -> u64;
    }
}

pub struct Echo;

impl<F: ReceivedResources> probe::Service<F> for Echo {
    fn ping(&mut self, _frame: &F, value: u64) -> u64 {
        value
    }
}

pub fn serve_probe(parent: Handle<kind::Port>, export: &'static Port) -> ! {
    let exporting = spawn_owned(
        move || {
            let parent = startup_contract::Client::new(PortTransport::new(parent));
            let _ = serve_export::<probe::Protocol>(&parent, whole_export(export));
            process_exit(EXPORT_STOPPED_EXIT_CODE)
        },
        EXPORT_STACK,
        Priority::new(1),
    );
    if exporting.is_err() {
        process_exit(EXPORT_STOPPED_EXIT_CODE)
    }

    let transport = PortTransport::new(export.as_raw());
    let mut bytes = [0u8; MESSAGE_INLINE_MAX];
    loop {
        match transport.recv(&mut bytes, Timeout::INFINITE) {
            Ok(frame) => {
                let _ = probe::dispatch(&mut Echo, frame);
            }
            Err(RecvError::SenderGone(_) | RecvError::Discarded | RecvError::Timeout) => {}
            Err(RecvError::Closed) => process_exit(EXPORT_STOPPED_EXIT_CODE),
        }
    }
}
