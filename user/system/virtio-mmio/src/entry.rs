//! Экспорт шины и обслуживание слотов одним executor.

use alloc::rc::Rc;
use core::{cell::RefCell, fmt};

use component_startup::{
    AsyncStartup, Requirements as _, SessionError, SessionPort, StartupError, component_manifest,
    serve_component,
};
use executor::{Executor, PortChannel};
use platform_abi::platform;
use syscall::{Handle, kind};
use virtio_mmio_abi::virtio_mmio;

use crate::bus::Bus;

component_manifest! {
    struct VirtioMmioStartup {
        requirements: { platform: session(platform::Protocol) },
        features: ["virtio,mmio"],
        exports: [virtio_mmio::Protocol],
    }
}

#[derive(Debug)]
enum Stopped {
    Startup(StartupError),
    Kernel(kernel_objects::Error),
    ServicePortClosed,
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => error.fmt(formatter),
            Self::Kernel(error) => error.fmt(formatter),
            Self::ServicePortClosed => formatter.write_str("service port closed"),
        }
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Stopped> {
    let requirements = VirtioMmioStartup::from_parent(parent);
    let platform = requirements.platform.open().map_err(Stopped::Startup)?;

    let executor = Rc::new(Executor::new().map_err(Stopped::Kernel)?);

    let platform = PortChannel::client(platform);

    let sessions = Rc::new(SessionPort::create().map_err(Stopped::Kernel)?);
    let bus = Rc::new(RefCell::new(None::<Rc<Bus>>));

    let dispatch = bus.clone();
    let opened = bus.clone();
    let closed = bus.clone();
    let port = sessions.clone();

    let served = serve_component::<virtio_mmio::Protocol, core::future::Ready<_>>(
        &executor,
        AsyncStartup::new(PortChannel::new(parent)),
        sessions,
        move |frame| {
            let bus = dispatch.borrow().clone();
            let bus = bus.ok_or(ipc::wire::IpcError::PeerClosed)?;
            bus.dispatch(frame).map(|()| None)
        },
        async move |_| {
            if opened.borrow().is_none() {
                let bus = Bus::take(&platform, port.clone())
                    .await
                    .ok_or(SessionError::Refused)?;

                *opened.borrow_mut() = Some(Rc::new(bus));
            }

            port.open_session()
        },
        move |badge| {
            if let Some(bus) = closed.borrow().as_ref() {
                bus.gone(badge);
            }
        },
    );

    let _ = executor.block_on(served);
    Err(Stopped::ServicePortClosed)
}
