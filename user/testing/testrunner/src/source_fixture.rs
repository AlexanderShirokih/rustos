//! Экспорт `InputSource` фикстур источников ввода. Источник объявляет одно устройство с одной
//! клавишей либо отказывает в подключении.

// Модуль подключён к прогону и к фикстурам, и каждый бинарь пользуется своим подмножеством.
#![allow(dead_code)]

use alloc::rc::Rc;

use component_startup::{
    AsyncStartup, SessionPort, component_startup as startup_contract, serve_component, whole_export,
};
use executor::{Executor, Mutex, PortChannel, Reactor};
use input_abi::{
    AttachError, AxisList, Binding, DeviceDescriptor, KeyList, Usage, input_inject, input_source,
};
use kernel_objects::{Delivery, ReceivedSession, Session, SessionTransport};
use runtime::{PortTransport, Timeout, process_exit};

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
    inject: Mutex<Option<input_inject::Client<PortChannel<SessionTransport>>>>,
    reactor: Reactor,
}

impl input_source::Service<Delivery> for Source {
    async fn attach(
        &self,
        _frame: &Delivery,
        inject: ReceivedSession<input_inject::Protocol>,
    ) -> Result<(), AttachError> {
        let mut attached = self.inject.lock().await;
        if attached.is_some() {
            return Err(AttachError::Busy);
        }

        let Attach::Declare(name) = self.attach else {
            return Err(AttachError::Refused);
        };

        let inject = PortChannel::client(Session::from_session(inject), self.reactor.clone())
            .with_wait(DECLARE_TIMEOUT);

        let mut keys = KeyList::new();
        keys.push(KEY).map_err(|_| AttachError::Refused)?;
        let axes = AxisList::new();
        let descriptor = DeviceDescriptor::new(name, Binding::None, &keys, &axes, 0, &axes)
            .ok_or(AttachError::Refused)?;

        inject
            .declare(descriptor)
            .await
            .map_err(|_| AttachError::Refused)?;

        *attached = Some(inject);
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
    let Ok(executor) = Executor::new() else {
        process_exit(1)
    };
    let executor = Rc::new(executor);
    let reactor = executor.reactor();

    let Ok(export) = SessionPort::create() else {
        process_exit(1)
    };
    let export = Rc::new(export);
    let opened = export.clone();

    let source = Rc::new(Source {
        attach,
        inject: Mutex::new(None),
        reactor: reactor.clone(),
    });
    let parent = AsyncStartup::new(PortChannel::from_transport(
        parent.into_channel(),
        reactor.clone(),
    ));

    let served = serve_component::<input_source::Protocol, _>(
        &executor,
        parent,
        export,
        move |frame| input_source::serve(&source, frame),
        whole_export(opened),
        |_| {},
    );

    executor.block_on(async {
        if let Some(delay) = delay {
            reactor.sleep(delay).await;
        }

        let _ = served.await;
    });
    process_exit(1)
}
