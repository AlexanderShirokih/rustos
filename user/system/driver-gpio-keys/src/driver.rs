//! Источник ввода `InputSource` узла `gpio-keys`: объявляет устройство кнопок узла, передаёт
//! начальное состояние клавиш и нажатия по прерываниям линий.

use alloc::{sync::Arc, vec::Vec};
use core::{convert::Infallible, fmt};

use component_startup::{
    AnyOf, Import, Providers, SessionEvent, SessionPort, StartupError,
    component_startup as startup_contract, serve_export, whole_export,
};
use driver_gpio_keys::{Edge, Key, KeysError, Window, keys};
use input_abi::{
    AttachError, AxisList, Binding, Command, DeclareError, DeviceDescriptor, DeviceNumber, KeyList,
    UpdateError, input_inject, input_source,
};
use ipc::CallError;
use kernel_objects::{Delivery, ReceivedSession, Session, SessionTransport, Signal, wait_any};
use pins_abi::{PinsError, controllers, device_pins, pins};
use platform_abi::{
    CONTROLLER_KEY, Description, DescriptionError, DeviceId, MappedDescription, PlatformError,
    platform,
};
use runtime::{
    FAILED_EXIT_CODE, Instant, PortTransport, Timeout, exit, log_fmt, spawn_bound, spawn_owned,
};
use syscall::{SIGNALED, SyscallError, WaitItem};

const COMPATIBLE: &str = "gpio-keys";
const UNNAMED_LINE: &str = "";

#[derive(Debug)]
pub(crate) enum Stopped {
    Startup(StartupError),
    ExportPort,
    ExportThread,
    ExportPortClosed,
}

struct Source {
    pins: Providers<pins::Protocol>,
    platform: platform::Client<SessionTransport>,
    node: Option<DeviceId>,
    attached: bool,
    refused: bool,
}

struct Line {
    number: u8,
    key: u16,
    signal: Signal,
    window: Window,
}

struct Reporter {
    pins: device_pins::Client<SessionTransport>,
    inject: input_inject::Client<SessionTransport>,
    device: DeviceNumber,
    held: Vec<usize>,
}

struct Buttons {
    _provider: pins::Client<SessionTransport>,
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
    Thread,
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => error.fmt(formatter),
            Self::ExportPort => formatter.write_str("export port refused"),
            Self::ExportThread => formatter.write_str("export thread refused"),
            Self::ExportPortClosed => formatter.write_str("export port closed"),
        }
    }
}

impl Source {
    fn bind(&mut self, inject: input_inject::Client<SessionTransport>) -> Result<(), Failure> {
        let description = self.platform.description().map_err(Failure::Platform)?;
        let mapped = MappedDescription::new(description)
            .map_err(|refused| Failure::Mapping(refused.error))?;
        let description = mapped.description().map_err(Failure::Description)?;

        let node = description
            .compatible_devices(COMPATIBLE)
            .next()
            .map(|device| device.id().clone())
            .ok_or(Failure::NoNode)?;
        let node = self.node.insert(node);

        let buttons = Buttons::bind(&description, node, &self.pins, inject)?;
        spawn_owned(move || buttons.watch())
            .map(drop)
            .map_err(|_| Failure::Thread)
    }
}

impl input_source::Service<Delivery> for Source {
    fn attach(
        &mut self,
        _frame: &Delivery,
        inject: ReceivedSession<input_inject::Protocol>,
    ) -> Result<(), AttachError> {
        if self.attached {
            return Err(AttachError::Busy);
        }

        let inject = Session::from_session(inject).client();

        let bound = self.bind(inject);
        let node = self.node.as_ref().map_or(COMPATIBLE, DeviceId::as_str);
        match bound {
            Ok(()) => {
                log_fmt(format_args!("{node} bound"));
                self.attached = true;
                Ok(())
            }
            Err(failure) => {
                log_fmt(format_args!("{node} not bound: {failure:?}"));
                self.refused = true;
                Err(AttachError::Refused)
            }
        }
    }
}

impl Buttons {
    fn bind(
        description: &Description<'_>,
        node: &DeviceId,
        pins: &Providers<pins::Protocol>,
        inject: input_inject::Client<SessionTransport>,
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

        let found = pins
            .all(AnyOf::new(CONTROLLER_KEY, compatible))
            .map_err(Failure::Startup)?
            .next()
            .ok_or(Failure::NoProvider)?
            .map_err(Failure::Startup)?;
        let provider = pins.open(&found).map_err(Failure::Startup)?.client();
        let device =
            Session::<device_pins::Protocol>::from_session(provider.open(node.clone())?).client();

        let lines = keys
            .keys
            .iter()
            .map(|key| Line::bind(&device, key))
            .collect::<Result<Vec<_>, _>>()?;

        let mut usages = KeyList::new();
        for usage in &keys.usages {
            usages.push(*usage).map_err(|_| Failure::Descriptor)?;
        }
        let axes = AxisList::new();
        let descriptor = DeviceDescriptor::new(keys.name, Binding::None, &usages, &axes, 0, &axes)
            .ok_or(Failure::Descriptor)?;
        let number = inject.declare(descriptor).map_err(Failure::Declare)?;

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

    fn watch(mut self) -> u32 {
        match self.serve() {
            Ok(never) => match never {},
            Err(failure) => stop(&failure),
        }
    }

    fn serve(&mut self) -> Result<Infallible, Failure> {
        for line in &mut self.lines {
            self.reporter.settle(line)?;
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
                    self.reporter.settle(line)?;
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

            match wait_any(&items, timeout) {
                Ok(index) => {
                    let now = Instant::now().as_nanos();
                    if let Some(line) = self.lines.get_mut(index) {
                        self.reporter.pins.ack(line.number)?;
                        line.window.interrupt(now);
                    }
                }
                Err(kernel_objects::Error::Syscall(SyscallError::Timeout)) => {}
                Err(error) => return Err(Failure::Wait(error)),
            }
        }
    }
}

impl Line {
    fn bind(device: &device_pins::Client<SessionTransport>, key: &Key) -> Result<Self, Failure> {
        let number = device.input(key.node.clone(), UNNAMED_LINE, 0)?;
        let signal = Signal::from(device.irq(number)?);

        Ok(Self {
            number,
            key: u16::try_from(key.usage).map_err(|_| Failure::Descriptor)?,
            signal,
            window: Window::new(key.debounce),
        })
    }
}

impl Reporter {
    fn settle(&mut self, line: &mut Line) -> Result<(), Failure> {
        let active = self.pins.active(line.number)?;
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
    parent: startup_contract::Client<PortTransport>,
) -> Result<(), Stopped> {
    let platform = platform.open().map_err(Stopped::Startup)?.client();

    let export = Arc::new(SessionPort::create().map_err(|_| Stopped::ExportPort)?);
    let port = Arc::clone(&export);
    spawn_bound(move || serve_export::<input_source::Protocol>(parent, whole_export(&port)))
        .map_err(|_| Stopped::ExportThread)?;

    let mut source = Source {
        pins,
        platform,
        node: None,
        attached: false,
        refused: false,
    };
    let mut events = export.events();

    while let Some(event) = events.recv() {
        if let SessionEvent::Frame(frame) = event {
            let _ = input_source::dispatch(&mut source, frame);
            if source.refused {
                exit(FAILED_EXIT_CODE)
            }
        }
    }

    Err(Stopped::ExportPortClosed)
}

fn stop(failure: &Failure) -> ! {
    log_fmt(format_args!("{COMPATIBLE} stopped: {failure:?}"));
    exit(FAILED_EXIT_CODE)
}
