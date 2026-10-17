//! Подключает NT36672A как источник `InputSource` и передаёт касания по прерываниям чипа.

use alloc::sync::Arc;
use core::{convert::Infallible, fmt, time::Duration};

use component_startup::{
    AnyOf, Import, Providers, SessionEvent, SessionPort, StartupError,
    component_startup as startup_contract, serve_export, whole_export,
};
use driver_novatek_nvt_ts::{
    Contacts, Device, DeviceError, PARAMETERS_ADDRESS, PARAMETERS_LENGTH, Parameters,
    ParametersError, REPORT_ADDRESS,
};
use i2c_abi::{I2cError, TransferError, i2c, i2c_device};
use input_abi::{
    AttachError, DeclareError, DeviceNumber, RecordsError, UpdateError, input_inject, input_source,
};
use ipc::{CallError, Protocol};
use kernel_objects::{Delivery, ReceivedSession, Session, SessionTransport, Signal};
use pins_abi::{PinsError, controllers, device_pins, pins};
use platform_abi::{
    CONTROLLER_KEY, Description, DescriptionError, DeviceId, MappedDescription, PlatformError,
    platform,
};
use runtime::{
    FAILED_EXIT_CODE, Instant, PortTransport, Timeout, exit, log_fmt, sleep, spawn_bound,
    spawn_owned,
};
use syscall::SIGNALED;

const COMPATIBLE: &str = "novatek,nt36672a-ts";
const RESET_LINE: &str = "reset";
const INTERRUPT_RECORD: u8 = 0;
const RESET_DELAY: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub(crate) enum Stopped {
    Startup(StartupError),
    ExportPort,
    ExportThread,
    ExportPortClosed,
}

struct Source {
    i2c: Providers<i2c::Protocol>,
    pins: Providers<pins::Protocol>,
    platform: platform::Client<SessionTransport>,
    node: Option<DeviceId>,
    attached: bool,
    refused: bool,
}

struct Touchscreen {
    _pins_provider: pins::Client<SessionTransport>,
    _bus: i2c::Client<SessionTransport>,
    pins: device_pins::Client<SessionTransport>,
    chip: i2c_device::Client<SessionTransport>,
    inject: input_inject::Client<SessionTransport>,
    device: DeviceNumber,
    interrupt: u8,
    signal: Signal,
    contacts: Contacts,
}

#[derive(Debug, PartialEq, Eq)]
enum Failure {
    Platform(CallError<PlatformError>),
    Mapping(kernel_objects::Error),
    Description(DescriptionError),
    NoNode,
    NoParent,
    State,
    Controllers(usize),
    NoController,
    NoProvider,
    Startup(StartupError),
    Pins(CallError<PinsError>),
    I2c(CallError<I2cError>),
    Parameters(CallError<TransferError>),
    Chip(ParametersError),
    Device(DeviceError),
    Descriptor,
    Declare(CallError<DeclareError>),
    Records(RecordsError),
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

        let touchscreen = Touchscreen::bind(&description, node, &self.i2c, &self.pins, inject)?;
        spawn_owned(move || touchscreen.watch())
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

impl Touchscreen {
    fn bind(
        description: &Description<'_>,
        node: &DeviceId,
        i2c: &Providers<i2c::Protocol>,
        pins: &Providers<pins::Protocol>,
        inject: input_inject::Client<SessionTransport>,
    ) -> Result<Self, Failure> {
        let states = controllers(description, node.as_str()).ok_or(Failure::State)?;
        let [controller] = states.as_slice() else {
            return Err(Failure::Controllers(states.len()));
        };
        let pins_provider = provider(pins, controller)?.client();
        let device_pins =
            Session::<device_pins::Protocol>::from_session(pins_provider.open(node.clone())?)
                .client();
        device_pins.output(node.clone(), RESET_LINE, 0, false)?;
        sleep(RESET_DELAY);

        let parent = description
            .ancestors(node)
            .next()
            .ok_or(Failure::NoParent)?;
        let bus = provider(i2c, &parent.node)?.client();
        let chip = Session::<i2c_device::Protocol>::from_session(
            bus.open(node.clone()).map_err(Failure::I2c)?,
        )
        .client();
        let parameters = chip
            .write_read(&[PARAMETERS_ADDRESS], PARAMETERS_LENGTH)
            .map_err(Failure::Parameters)?;
        let parameters = Parameters::parse(&parameters.0).map_err(Failure::Chip)?;

        let described = description.node(node).ok_or(Failure::NoNode)?;
        let device = Device::new(&described, parameters).map_err(Failure::Device)?;
        let interrupt = device_pins.interrupt(node.clone(), INTERRUPT_RECORD)?;
        let signal = Signal::from(device_pins.irq(interrupt)?);

        let descriptor = device.descriptor().ok_or(Failure::Descriptor)?;
        let number = inject.declare(descriptor).map_err(Failure::Declare)?;

        Ok(Self {
            _pins_provider: pins_provider,
            _bus: bus,
            pins: device_pins,
            chip,
            inject,
            device: number,
            interrupt,
            signal,
            contacts: Contacts::new(&device),
        })
    }

    fn watch(mut self) -> u32 {
        match self.serve() {
            Ok(never) => match never {},
            Err(failure) => stop(&failure),
        }
    }

    fn serve(&mut self) -> Result<Infallible, Failure> {
        let mut read_refused = false;
        loop {
            self.signal
                .wait(SIGNALED, Timeout::INFINITE)
                .map_err(Failure::Wait)?;
            let time = Instant::now().as_nanos();

            match self
                .chip
                .write_read(&[REPORT_ADDRESS], self.contacts.report_length())
            {
                Ok(report) => {
                    let command = self
                        .contacts
                        .translate(&report.0)
                        .map_err(Failure::Records)?;
                    if !command.as_bytes().is_empty() {
                        self.inject
                            .update(self.device, Some(time), command.as_bytes())
                            .map_err(Failure::Update)?;
                    }
                }
                Err(error) => {
                    if !read_refused {
                        log_fmt(format_args!("{COMPATIBLE} report not read: {error:?}"));
                        read_refused = true;
                    }
                }
            }

            self.pins.ack(self.interrupt)?;
        }
    }
}

impl From<CallError<PinsError>> for Failure {
    fn from(error: CallError<PinsError>) -> Self {
        Self::Pins(error)
    }
}

pub(crate) fn run(
    i2c: Providers<i2c::Protocol>,
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
        i2c,
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

fn provider<P: Protocol>(
    providers: &Providers<P>,
    controller: &platform_abi::Node<'_>,
) -> Result<Session<P>, Failure> {
    let compatible = controller
        .strings("compatible")
        .map_err(Failure::Description)?
        .ok_or(Failure::NoController)?;
    let found = providers
        .all(AnyOf::new(CONTROLLER_KEY, compatible))
        .map_err(Failure::Startup)?
        .next()
        .ok_or(Failure::NoProvider)?
        .map_err(Failure::Startup)?;
    providers.open(&found).map_err(Failure::Startup)
}

fn stop(failure: &Failure) -> ! {
    log_fmt(format_args!("{COMPATIBLE} stopped: {failure:?}"));
    exit(FAILED_EXIT_CODE)
}
