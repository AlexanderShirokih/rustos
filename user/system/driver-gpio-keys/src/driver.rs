//! Источник ввода `InputSource` узла `gpio-keys`: объявляет устройство кнопок узла, передаёт
//! начальное состояние клавиш и нажатия по прерываниям линий.

use alloc::{
    rc::{Rc, Weak},
    vec::Vec,
};
use core::{
    cell::{Cell, RefCell},
    convert::Infallible,
    fmt,
};

use component_startup::{
    AnyOf, AsyncStartup, Import, Providers, SessionPort, StartupError, serve_component,
    whole_export,
};
use driver_gpio_keys::{Edge, Key, KeysError, Window, keys};
use executor::{Executor, Mutex, PortChannel};
use input_abi::{
    AttachError, AxisList, Binding, Command, DeclareError, DeviceDescriptor, DeviceNumber, KeyList,
    UpdateError, input_inject, input_source,
};
use ipc::CallError;
use kernel_objects::{Delivery, ReceivedSession, Session, SessionTransport, Signal};
use pins_abi::{PinsError, controllers, device_pins, pins};
use platform_abi::{
    CONTROLLER_KEY, Description, DescriptionError, DeviceId, MappedDescription, PlatformError,
    platform,
};
use runtime::{FAILED_EXIT_CODE, Instant, Timeout, exit, log_fmt};
use syscall::{Handle, SIGNALED, WaitItem, kind};

type Channel = PortChannel<SessionTransport>;

const COMPATIBLE: &str = "gpio-keys";
const UNNAMED_LINE: &str = "";

#[derive(Debug)]
pub(crate) enum Stopped {
    Startup(StartupError),
    ExportPort,
    Executor,
    ExportPortClosed,
}

struct Source {
    pins: Providers<pins::Protocol>,
    platform: platform::Client<Channel>,
    node: RefCell<Option<DeviceId>>,
    attached: Mutex<bool>,
    refused: Cell<bool>,
    executor: Weak<Executor>,
}

struct Line {
    number: u8,
    key: u16,
    signal: Signal,
    window: Window,
}

struct Reporter {
    pins: device_pins::Client<Channel>,
    inject: input_inject::Client<Channel>,
    device: DeviceNumber,
    held: Vec<usize>,
}

struct Buttons {
    _provider: pins::Client<Channel>,
    reporter: Reporter,
    lines: Vec<Line>,
}

#[derive(Debug, PartialEq, Eq)]
enum Failure {
    Platform(CallError<PlatformError>),
    Mapping(kernel_objects::Error),
    Description(DescriptionError),
    NoNode,
    Keys(KeysError),
    State,
    Controllers(usize),
    NoController,
    NoProvider,
    Startup(StartupError),
    Pins(CallError<PinsError>),
    Descriptor,
    Declare(CallError<DeclareError>),
    Update(CallError<UpdateError>),
    Wait(kernel_objects::Error),
    Executor,
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => error.fmt(formatter),
            Self::ExportPort => formatter.write_str("export port refused"),
            Self::Executor => formatter.write_str("executor refused"),
            Self::ExportPortClosed => formatter.write_str("export port closed"),
        }
    }
}

impl Source {
    async fn bind(&self, inject: input_inject::Client<Channel>) -> Result<(), Failure> {
        let description = self
            .platform
            .description()
            .await
            .map_err(Failure::Platform)?;

        let mapped = MappedDescription::new(description)
            .map_err(|refused| Failure::Mapping(refused.error))?;
        let description = mapped.description().map_err(Failure::Description)?;

        let node = description
            .compatible_devices(COMPATIBLE)
            .next()
            .map(|device| device.id().clone())
            .ok_or(Failure::NoNode)?;
        *self.node.borrow_mut() = Some(node.clone());

        let buttons = Buttons::bind(&description, &node, &self.pins, inject).await?;

        self.executor
            .upgrade()
            .ok_or(Failure::Executor)?
            .spawn(buttons.watch());

        Ok(())
    }
}

impl input_source::Service<Delivery> for Source {
    async fn attach(
        &self,
        _frame: &Delivery,
        inject: ReceivedSession<input_inject::Protocol>,
    ) -> Result<(), AttachError> {
        let mut attached = self.attached.lock().await;
        if *attached {
            return Err(AttachError::Busy);
        }

        let inject = PortChannel::client(Session::from_session(inject));

        let bound = self.bind(inject).await;
        let node = self.node.borrow();
        let node = node.as_ref().map_or(COMPATIBLE, DeviceId::as_str);
        match bound {
            Ok(()) => {
                log_fmt(format_args!("{node} bound"));
                *attached = true;
                Ok(())
            }

            Err(failure) => {
                log_fmt(format_args!("{node} not bound: {failure:?}"));
                self.refused.set(true);
                Err(AttachError::Refused)
            }
        }
    }
}

impl Buttons {
    async fn bind(
        description: &Description<'_>,
        node: &DeviceId,
        pins: &Providers<pins::Protocol>,
        inject: input_inject::Client<Channel>,
    ) -> Result<Self, Failure> {
        let states = controllers(description, node.as_str()).ok_or(Failure::State)?;
        let [controller] = states.as_slice() else {
            return Err(Failure::Controllers(states.len()));
        };
        let compatible = controller
            .strings("compatible")
            .map_err(Failure::Description)?
            .ok_or(Failure::NoController)?;

        let keys = keys(description, node).map_err(Failure::Keys)?;

        let session = pins
            .open_first(AnyOf::new(CONTROLLER_KEY, compatible))
            .await
            .map_err(Failure::Startup)?
            .ok_or(Failure::NoProvider)?;

        let provider = PortChannel::client(session);

        let device = PortChannel::client(Session::<device_pins::Protocol>::from_session(
            provider.open(node.clone()).await?,
        ));

        let mut lines = Vec::new();
        for key in &keys.keys {
            lines.push(Line::bind(&device, key).await?);
        }

        let mut usages = KeyList::new();
        for usage in &keys.usages {
            usages.push(*usage).map_err(|_| Failure::Descriptor)?;
        }

        let axes = AxisList::new();
        let descriptor = DeviceDescriptor::new(keys.name, Binding::None, &usages, &axes, 0, &axes)
            .ok_or(Failure::Descriptor)?;

        let number = inject.declare(descriptor).await.map_err(Failure::Declare)?;

        Ok(Self {
            _provider: provider,
            reporter: Reporter {
                pins: device,
                inject,
                device: number,
                held: alloc::vec![0; keys.usages.len()],
            },
            lines,
        })
    }

    async fn watch(mut self) {
        match self.serve().await {
            Ok(never) => match never {},
            Err(failure) => stop(&failure),
        }
    }

    async fn serve(&mut self) -> Result<Infallible, Failure> {
        for line in &mut self.lines {
            self.reporter.settle(line).await?;
        }

        let items: Vec<WaitItem> = self
            .lines
            .iter()
            .map(|line| WaitItem::new(line.signal.as_raw(), SIGNALED))
            .collect();

        loop {
            let now = Instant::now().as_nanos();
            for line in &mut self.lines {
                if line
                    .window
                    .deadline()
                    .is_some_and(|deadline| deadline <= now)
                {
                    self.reporter.settle(line).await?;
                }
            }

            let timeout = self
                .lines
                .iter()
                .filter_map(|line| line.window.deadline())
                .min()
                .map_or(Timeout::INFINITE, |deadline| {
                    Timeout::from_ns(deadline.saturating_sub(Instant::now().as_nanos()))
                });

            match executor::timeout(timeout, executor::wait_any(&items)).await {
                Some(Ok(index)) => {
                    let now = Instant::now().as_nanos();
                    if let Some(line) = self.lines.get_mut(index) {
                        self.reporter.pins.ack(line.number).await?;
                        line.window.interrupt(now);
                    }
                }
                None => {}
                Some(Err(error)) => return Err(Failure::Wait(error)),
            }
        }
    }
}

impl Line {
    async fn bind(device: &device_pins::Client<Channel>, key: &Key) -> Result<Self, Failure> {
        let number = device.input(key.node.clone(), UNNAMED_LINE, 0).await?;
        let signal = Signal::from(device.irq(number).await?);

        Ok(Self {
            number,
            key: u16::try_from(key.usage).map_err(|_| Failure::Descriptor)?,
            signal,
            window: Window::new(key.debounce),
        })
    }
}

impl Reporter {
    async fn settle(&mut self, line: &mut Line) -> Result<(), Failure> {
        let active = self.pins.active(line.number).await?;
        let time = Instant::now().as_nanos();
        let Some(edge) = line.window.settle(active) else {
            return Ok(());
        };

        let held = self
            .held
            .get_mut(usize::from(line.key))
            .ok_or(Failure::Descriptor)?;
        let mut command = Command::new();
        let built = match edge {
            Edge::Press => {
                *held = held.saturating_add(1);
                if *held > 1 {
                    return Ok(());
                }
                command.press(line.key)
            }
            Edge::Release => {
                *held = held.saturating_sub(1);
                if *held > 0 {
                    return Ok(());
                }
                command.release(line.key)
            }
        };

        built.map_err(|_| Failure::Descriptor)?;

        self.inject
            .update(self.device, Some(time), command.as_bytes())
            .await
            .map_err(Failure::Update)
    }
}

impl From<CallError<PinsError>> for Failure {
    fn from(error: CallError<PinsError>) -> Self {
        Self::Pins(error)
    }
}

pub(crate) fn run(
    pins: Providers<pins::Protocol>,
    platform: &Import<platform::Protocol>,
    parent: Handle<kind::Port>,
) -> Result<(), Stopped> {
    let executor = Rc::new(Executor::new().map_err(|_| Stopped::Executor)?);

    let platform = PortChannel::client(platform.open().map_err(Stopped::Startup)?);

    let export = Rc::new(SessionPort::create().map_err(|_| Stopped::ExportPort)?);
    let source = Rc::new(Source {
        pins,
        platform,
        node: RefCell::new(None),
        attached: Mutex::new(false),
        refused: Cell::new(false),
        executor: Rc::downgrade(&executor),
    });

    let opened = export.clone();

    let served = serve_component::<input_source::Protocol, _>(
        &executor,
        AsyncStartup::new(PortChannel::new(parent)),
        export,
        move |frame| {
            let call = input_source::serve(&source, frame)?;
            let source = source.clone();
            Ok(call.map(|call| async move {
                let outcome = call.await;
                if source.refused.get() {
                    exit(FAILED_EXIT_CODE);
                }
                outcome
            }))
        },
        whole_export(opened),
        |_| {},
    );

    let _ = executor.block_on(served);
    Err(Stopped::ExportPortClosed)
}

fn stop(failure: &Failure) -> ! {
    log_fmt(format_args!("{COMPATIBLE} stopped: {failure:?}"));
    exit(FAILED_EXIT_CODE)
}
