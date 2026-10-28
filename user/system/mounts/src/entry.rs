//! Обслуживание Namespace и FileSystemMount одним executor.

use alloc::rc::Rc;
use core::{fmt, future::Future, task::Poll};

use bootstrap_abi::image;
use component_startup::{
    AsyncStartup, Requirements as _, SessionPort, component_manifest, serve_component,
};
use executor::{Executor, PortChannel};
use filesystem_abi::{file_system, file_system_mount, namespace};
use syscall::{Handle, kind};

use crate::{router::Router, table};

component_manifest! {
    struct RouterStartup {
        requirements: {
            image: session(image::Protocol),
            filesystem: session(file_system::Protocol),
        },
        exports: [namespace::Protocol, file_system_mount::Protocol],
    }
}

#[derive(Debug)]
enum Stopped {
    Runtime(kernel_objects::Error),
    ServicePortClosed,
    MountPortClosed,
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime(error) => error.fmt(formatter),
            Self::ServicePortClosed => formatter.write_str("service port closed"),
            Self::MountPortClosed => formatter.write_str("mount port closed"),
        }
    }
}

impl From<kernel_objects::Error> for Stopped {
    fn from(error: kernel_objects::Error) -> Self {
        Self::Runtime(error)
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Stopped> {
    let requirements = RouterStartup::from_parent(parent);

    let executor = Rc::new(Executor::new()?);
    let views = Rc::new(SessionPort::create()?);
    let mounts = Rc::new(SessionPort::create()?);
    let router = Rc::new(Router::new(views.clone(), mounts.clone()));

    executor.block_on(async {
        let lines = table::read(&requirements.image).await;
        table::start(lines, parent, &router, &executor);

        let dispatch = router.clone();
        let opened = router.clone();
        let closed = router.clone();
        let views = serve_component::<namespace::Protocol, _>(
            &executor,
            AsyncStartup::new(PortChannel::new(parent)),
            views,
            move |frame| namespace::serve(&dispatch, frame),
            async move |_| opened.open_view(),
            move |badge| closed.view_gone(badge),
        );

        let dispatch = router.clone();
        let opened = router.clone();
        let closed = router.clone();
        let mounts = serve_component::<file_system_mount::Protocol, _>(
            &executor,
            AsyncStartup::new(PortChannel::new(parent)),
            mounts,
            move |frame| file_system_mount::serve(&dispatch, frame),
            async move |_| opened.open_mount(),
            move |badge| closed.mount_gone(badge),
        );

        let mut views = core::pin::pin!(views);
        let mut mounts = core::pin::pin!(mounts);
        let stopped = core::future::poll_fn(|cx| {
            if views.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Stopped::ServicePortClosed);
            }

            if mounts.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Stopped::MountPortClosed);
            }

            Poll::Pending
        })
        .await;
        Err(stopped)
    })
}
