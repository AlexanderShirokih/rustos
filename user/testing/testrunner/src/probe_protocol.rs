//! Экспорт `Probe` фикстур композиции.

// Модуль подключён к прогону и к фикстурам, и каждый бинарь пользуется своим подмножеством.
#![allow(dead_code)]

use core::fmt;

use component_abi::{Attributes, component_startup as startup_contract};
use component_startup::{accept, whole_export};
use ipc::{Listener as _, ReceivedResources, RecvError, wire::MESSAGE_INLINE_MAX};
use kernel_objects::Port;
use runtime::{PortTransport, Timeout, log_fmt, process_exit, spawn_owned};
use syscall::{Handle, kind};

ipc::protocol! {
    pub mod probe {
        name = "Probe";

        call fn ping(value: u64) -> u64;
    }
}

pub(crate) struct Echo;

pub(crate) struct Pairs<'a>(pub(crate) Attributes<'a>);

impl<F: ReceivedResources> probe::Service<F> for Echo {
    fn ping(&mut self, _frame: &F, value: u64) -> u64 {
        value
    }
}

impl fmt::Display for Pairs<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, pair) in self.0.iter().enumerate() {
            if index > 0 {
                formatter.write_str(",")?;
            }

            let key = core::str::from_utf8(pair.key).map_err(|_| fmt::Error)?;
            let value = core::str::from_utf8(pair.value).map_err(|_| fmt::Error)?;
            write!(formatter, "{key}={value}")?;
        }
        Ok(())
    }
}

pub(crate) fn serve_probe(parent: Handle<kind::Port>, export: &'static Port) -> ! {
    let exporting = spawn_owned(move || {
        let parent = startup_contract::Client::new(PortTransport::new(parent));
        let mut open = whole_export(export);
        while let Ok(request) = accept::<probe::Protocol>(&parent) {
            let attributes = request.attributes.as_attributes();
            if !attributes.is_empty() {
                log_fmt(format_args!("request {}", Pairs(attributes)));
            }

            let _ = request.reply.send(open());
        }
        process_exit(1)
    });
    if exporting.is_err() {
        process_exit(1)
    }

    let transport = PortTransport::new(export.as_raw());
    let mut bytes = [0u8; MESSAGE_INLINE_MAX];
    loop {
        match transport.recv(&mut bytes, Timeout::INFINITE) {
            Ok(frame) => {
                let _ = probe::dispatch(&mut Echo, frame);
            }
            Err(RecvError::SenderGone(_) | RecvError::Discarded | RecvError::Timeout) => {}
            Err(RecvError::Closed) => process_exit(1),
        }
    }
}
