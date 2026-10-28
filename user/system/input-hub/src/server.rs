use alloc::{boxed::Box, rc::Rc};
use core::{cell::RefCell, fmt};

use component_startup::{
    AsyncStartup, Requirements as _, ServeStopped, SessionPort, StartupError, component_manifest,
    serve_component, whole_export,
};
use executor::{Executor, PortChannel};
use input_abi::{input, input_focus, input_inject, input_source};
use syscall::{Handle, SyscallError, kind};

use crate::{
    hub::{Consumer, Holder, Hub, Source, open_input},
    sources::attach_sources,
};

component_manifest! {
    struct HubStartup {
        requirements: { sources: all(input_source::Protocol) },
        exports: [input_inject::Protocol, input::Protocol, input_focus::Protocol],
    }
}

#[derive(Debug)]
enum Stopped {
    Runtime(kernel_objects::Error),
    Sessions(ServeStopped),
    SourceWatch(StartupError),
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime(error) => error.fmt(formatter),
            Self::Sessions(error) => error.fmt(formatter),
            Self::SourceWatch(error) => write!(formatter, "source watch: {error}"),
        }
    }
}

impl From<kernel_objects::Error> for Stopped {
    fn from(error: kernel_objects::Error) -> Self {
        Self::Runtime(error)
    }
}

impl From<SyscallError> for Stopped {
    fn from(error: SyscallError) -> Self {
        Self::Runtime(error.into())
    }
}

impl From<ServeStopped> for Stopped {
    fn from(stopped: ServeStopped) -> Self {
        Self::Sessions(stopped)
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Stopped> {
    let requirements = HubStartup::from_parent(parent);
    let sources = Rc::new(SessionPort::create()?);
    let consumers = Rc::new(SessionPort::create()?);
    let holders = Rc::new(SessionPort::create()?);

    let executor = Rc::new(Executor::new()?);
    let hub = Rc::new(RefCell::new(Hub::new()));
    let startup = || AsyncStartup::new(PortChannel::new(parent));

    let source = Rc::new(Source { hub: hub.clone() });
    let source_gone = source.clone();
    executor.spawn(stop_on(serve_component::<input_inject::Protocol, _>(
        &executor,
        startup(),
        sources.clone(),
        move |frame| input_inject::serve(&source, frame),
        whole_export(sources.clone()),
        move |badge| source_gone.gone(badge),
    )));

    let holder = Rc::new(Holder {
        hub: hub.clone(),
        consumers: consumers.clone(),
    });
    let holder_gone = holder.clone();
    executor.spawn(stop_on(serve_component::<input_focus::Protocol, _>(
        &executor,
        startup(),
        holders.clone(),
        move |frame| input_focus::serve(&holder, frame),
        whole_export(holders),
        move |badge| holder_gone.gone(badge),
    )));

    executor.spawn(stop_on(async move {
        Stopped::SourceWatch(Box::pin(attach_sources(&requirements.sources, &sources)).await)
    }));

    let consumer = Rc::new(Consumer { hub: hub.clone() });
    let gone = consumer.clone();
    let input_export = consumers.clone();

    let served = serve_component::<input::Protocol, _>(
        &executor,
        startup(),
        consumers,
        move |frame| Consumer::serve(&consumer, frame),
        async move |_| open_input(&hub, &input_export),
        move |badge| gone.gone(badge),
    );

    Err(Stopped::Sessions(executor.block_on(served)))
}

async fn stop_on(task: impl Future<Output = impl Into<Stopped>>) {
    runtime::finish(Err::<(), _>(task.await.into()))
}
