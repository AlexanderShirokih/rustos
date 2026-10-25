//! Экспорт `Probe` фикстур композиции.

// Модуль подключён к прогону и к фикстурам, и каждый бинарь пользуется своим подмножеством.
#![allow(dead_code)]

extern crate alloc;

use alloc::rc::Rc;
use core::fmt;

use component_abi::Attributes;
use component_startup::{AsyncStartup, SessionPort, serve_component};
use executor::{Executor, PortChannel};
use ipc::ReceivedResources;
use runtime::{log_fmt, process_exit};
use syscall::{Handle, kind};

ipc::protocol! {
    pub mod probe {
        name = "Probe";

        call fn ping(value: u64) -> u64;
    }
}

pub(crate) struct Echo;

pub(crate) struct Pairs<'a>(pub(crate) Attributes<'a>);

#[allow(clippy::unused_async_trait_impl)]
impl<F: ReceivedResources> probe::Service<F> for Echo {
    fn ping(&self, _frame: &F, value: u64) -> u64 {
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

pub(crate) async fn serve_probe(
    executor: &Rc<Executor>,
    parent: Handle<kind::Port>,
    export: SessionPort,
) -> ! {
    let export = Rc::new(export);
    let opened = export.clone();
    let parent = AsyncStartup::new(PortChannel::new(parent, executor.reactor()));
    let calls = Rc::new(Echo);

    let served = serve_component::<probe::Protocol, _>(
        executor,
        parent,
        export,
        move |frame| probe::serve(&calls, frame),
        async move |attributes| {
            if !attributes.is_empty() {
                log_fmt(format_args!("request {}", Pairs(attributes)));
            }

            opened.open_session()
        },
        |_| {},
    );

    let _ = served.await;
    process_exit(1)
}
