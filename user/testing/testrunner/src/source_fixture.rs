//! Экспорт `InputSource` фикстур источников ввода. Источник объявляет одно устройство с одной
//! клавишей либо отказывает в подключении.

// Модуль подключён к прогону и к фикстурам, и каждый бинарь пользуется своим подмножеством.
#![allow(dead_code)]

use alloc::boxed::Box;

use component_startup::{
    SessionEvent, SessionEvents, component_startup as startup_contract, serve_export, whole_export,
};
use input_abi::{
    AttachError, AxisList, Binding, DeviceDescriptor, KeyList, Usage, input_inject, input_source,
};
use ipc::wire::Session;
use kernel_objects::{Delivery, IssuedHandle, Port, SessionTransport, kind};
use runtime::{PortTransport, Timeout, process_exit, spawn_owned};

pub(crate) const SOURCE_DEVICE: &str = "source";
pub(crate) const SLOW_DEVICE: &str = "slow";

const KEY: Usage = Usage::new(0x07, 0x04);

const DECLARE_TIMEOUT: Timeout = Timeout::from_secs(2);

pub(crate) enum Attach {
    Declare(&'static str),
    Refuse,
}

struct Source {
    attach: Attach,
    inject: Option<input_inject::Client<SessionTransport>>,
}

impl input_source::Service<Delivery> for Source {
    fn attach(
        &mut self,
        _frame: &Delivery,
        inject: Session<input_inject::Protocol, IssuedHandle<kind::Port>>,
    ) -> Result<(), AttachError> {
        if self.inject.is_some() {
            return Err(AttachError::Busy);
        }

        let Attach::Declare(name) = self.attach else {
            return Err(AttachError::Refused);
        };

        let inject = kernel_objects::Session::from_session(inject)
            .client()
            .with_wait(DECLARE_TIMEOUT);
        let declared = with_descriptor(name, |descriptor| inject.declare(descriptor).is_ok());
        if declared != Some(true) {
            return Err(AttachError::Refused);
        }

        self.inject = Some(inject);
        Ok(())
    }
}

pub(crate) fn with_descriptor<T>(
    name: &str,
    inspect: impl FnOnce(DeviceDescriptor<'_>) -> T,
) -> Option<T> {
    with_key_descriptor(name, KEY, inspect)
}

pub(crate) fn with_key_descriptor<T>(
    name: &str,
    key: Usage,
    inspect: impl FnOnce(DeviceDescriptor<'_>) -> T,
) -> Option<T> {
    let mut keys = KeyList::new();
    keys.push(key).ok()?;
    let axes = AxisList::new();
    let descriptor = DeviceDescriptor::new(name, Binding::None, &keys, &axes, 0, &axes)?;
    Some(inspect(descriptor))
}

pub(crate) fn serve_source(
    parent: startup_contract::Client<PortTransport>,
    attach: Attach,
    delay: Option<Timeout>,
) -> ! {
    let Ok(export) = Port::create() else {
        process_exit(1)
    };
    let export: &'static Port = Box::leak(Box::new(export));

    let exporting = spawn_owned(move || {
        if let Some(delay) = delay
            && let Ok(idle) = Port::create()
        {
            let _ = idle.recv(delay);
        }

        let _ = serve_export::<input_source::Protocol>(&parent, whole_export(export));
        process_exit(1)
    });
    if exporting.is_err() {
        process_exit(1)
    }

    let mut source = Source {
        attach,
        inject: None,
    };
    let mut events = SessionEvents::new(export);

    while let Some(event) = events.recv() {
        if let SessionEvent::Frame(frame) = event {
            let _ = input_source::dispatch(&mut source, frame);
        }
    }

    process_exit(1)
}
