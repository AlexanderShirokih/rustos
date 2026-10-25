use alloc::rc::Rc;
use core::cell::Cell;

use component_startup::{
    AsyncStartup, SessionPort, component_manifest, serve_component, whole_export,
};
use executor::{Executor, PortChannel, Reactor};
use kernel_objects::{Delivery, ReceivedSession, Session};
use syscall::{Handle, kind};

use crate::{
    async_component_protocol::{async_probe, slow_bus},
    failed::Failed,
};

component_manifest! { exports: [async_probe::Protocol], }

struct Service {
    reactor: Reactor,
    active: Cell<u32>,
}

struct InFlight<'a>(&'a Cell<u32>);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

#[allow(clippy::unused_async_trait_impl)]
impl async_probe::Service<Delivery> for Service {
    async fn slow(
        &self,
        _frame: &Delivery,
        bus: ReceivedSession<slow_bus::Protocol>,
    ) -> Result<u64, u8> {
        self.active.set(self.active.get() + 1);
        let _in_flight = InFlight(&self.active);
        let bus = Session::from_session(bus);
        slow_bus::Client::new(PortChannel::new(bus.handle(), self.reactor.clone()))
            .wait()
            .await
            .map_err(|_| 1)
    }

    fn quick(&self, _frame: &Delivery) -> u32 {
        42
    }

    fn active(&self, _frame: &Delivery) -> u32 {
        self.active.get()
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Failed> {
    let executor = Rc::new(Executor::new().map_err(|_| Failed)?);
    let sessions = Rc::new(SessionPort::create().map_err(|_| Failed)?);
    let service = Rc::new(Service {
        reactor: executor.reactor(),
        active: Cell::new(0),
    });
    let export = sessions.clone();

    let served = serve_component::<async_probe::Protocol, _>(
        &executor,
        AsyncStartup::new(PortChannel::new(parent, executor.reactor())),
        sessions,
        move |frame| async_probe::serve(&service, frame),
        whole_export(export),
        |_| {},
    );

    let _ = executor.block_on(served);
    Err(Failed)
}
