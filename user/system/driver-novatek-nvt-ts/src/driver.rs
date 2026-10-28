//! Подключает NT36672A как источник `InputSource` и передаёт касания по прерываниям чипа.

use alloc::rc::{Rc, Weak};
use core::{
    cell::{Cell, RefCell},
    convert::Infallible,
    fmt,
};

use component_startup::{
    AnyOf, AsyncStartup, Import, Providers, SessionPort, StartupError, serve_component,
    whole_export,
};
use driver_novatek_nvt_ts::{
    Contacts, Device, DeviceError, PARAMETERS_ADDRESS, PARAMETERS_LENGTH, Parameters,
    ParametersError, REPORT_ADDRESS,
};
use executor::{Executor, Mutex, PortChannel};
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
use runtime::{FAILED_EXIT_CODE, Instant, Timeout, exit, log_fmt};
use syscall::{Handle, SIGNALED, WaitItem, kind};

type Channel = PortChannel<SessionTransport>;

const COMPATIBLE: &str = "novatek,nt36672a-ts";
const RESET_LINE: &str = "reset";
const INTERRUPT_RECORD: u8 = 0;
const RESET_DELAY: Timeout = Timeout::from_millis(100);

#[derive(Debug)]
pub(crate) enum Stopped {
    Startup(StartupError),
    ExportPort,
    Executor,
    ExportPortClosed,
}

struct Source {
    i2c: Providers<i2c::Protocol>,
    pins: Providers<pins::Protocol>,
    platform: platform::Client<Channel>,
    node: RefCell<Option<DeviceId>>,
    attached: Mutex<bool>,
    refused: Cell<bool>,
    executor: Weak<Executor>,
}

struct Touchscreen {
    _pins_provider: pins::Client<Channel>,
    _bus: i2c::Client<Channel>,
    pins: device_pins::Client<Channel>,
    chip: i2c_device::Client<Channel>,
    inject: input_inject::Client<Channel>,
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

        let touchscreen =
            Touchscreen::bind(&description, &node, &self.i2c, &self.pins, inject).await?;
        self.executor
            .upgrade()
            .ok_or(Failure::Executor)?
            .spawn(touchscreen.watch());

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

impl Touchscreen {
    async fn bind(
        description: &Description<'_>,
        node: &DeviceId,
        i2c: &Providers<i2c::Protocol>,
        pins: &Providers<pins::Protocol>,
        inject: input_inject::Client<Channel>,
    ) -> Result<Self, Failure> {
        let states = controllers(description, node.as_str()).ok_or(Failure::State)?;
        let [controller] = states.as_slice() else {
            return Err(Failure::Controllers(states.len()));
        };

        let pins_provider = PortChannel::client(provider(pins, controller).await?);
        let device_pins = PortChannel::client(Session::<device_pins::Protocol>::from_session(
            pins_provider.open(node.clone()).await?,
        ));
        device_pins
            .output(node.clone(), RESET_LINE, 0, false)
            .await?;

        executor::sleep(RESET_DELAY).await;

        let parent = description
            .ancestors(node)
            .next()
            .ok_or(Failure::NoParent)?;
        let bus = PortChannel::client(provider(i2c, &parent.node).await?);

        let chip = PortChannel::client(Session::<i2c_device::Protocol>::from_session(
            bus.open(node.clone()).await.map_err(Failure::I2c)?,
        ));

        let parameters = chip
            .write_read(&[PARAMETERS_ADDRESS], PARAMETERS_LENGTH)
            .await
            .map_err(Failure::Parameters)?;
        let parameters = Parameters::parse(&parameters.0).map_err(Failure::Chip)?;

        let described = description.node(node).ok_or(Failure::NoNode)?;
        let device = Device::new(&described, parameters).map_err(Failure::Device)?;
        let interrupt = device_pins
            .interrupt(node.clone(), INTERRUPT_RECORD)
            .await?;

        let signal = Signal::from(device_pins.irq(interrupt).await?);

        let descriptor = device.descriptor().ok_or(Failure::Descriptor)?;
        let number = inject.declare(descriptor).await.map_err(Failure::Declare)?;

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

    async fn watch(mut self) {
        match self.serve().await {
            Ok(never) => match never {},
            Err(failure) => stop(&failure),
        }
    }

    async fn serve(&mut self) -> Result<Infallible, Failure> {
        let mut read_refused = false;
        loop {
            executor::wait(WaitItem::new(self.signal.as_raw(), SIGNALED))
                .await
                .map_err(Failure::Wait)?;
            let time = Instant::now().as_nanos();

            match self
                .chip
                .write_read(&[REPORT_ADDRESS], self.contacts.report_length())
                .await
            {
                Ok(report) => {
                    let command = self
                        .contacts
                        .translate(&report.0)
                        .map_err(Failure::Records)?;
                    if !command.as_bytes().is_empty() {
                        self.inject
                            .update(self.device, Some(time), command.as_bytes())
                            .await
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

            self.pins.ack(self.interrupt).await?;
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
    parent: Handle<kind::Port>,
) -> Result<(), Stopped> {
    let executor = Rc::new(Executor::new().map_err(|_| Stopped::Executor)?);

    let platform = PortChannel::client(platform.open().map_err(Stopped::Startup)?);

    let export = Rc::new(SessionPort::create().map_err(|_| Stopped::ExportPort)?);
    let source = Rc::new(Source {
        i2c,
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

async fn provider<P: Protocol>(
    providers: &Providers<P>,
    controller: &platform_abi::Node<'_>,
) -> Result<Session<P>, Failure> {
    let compatible = controller
        .strings("compatible")
        .map_err(Failure::Description)?
        .ok_or(Failure::NoController)?;

    providers
        .open_first(AnyOf::new(CONTROLLER_KEY, compatible))
        .await
        .map_err(Failure::Startup)?
        .ok_or(Failure::NoProvider)
}

fn stop(failure: &Failure) -> ! {
    log_fmt(format_args!("{COMPATIBLE} stopped: {failure:?}"));
    exit(FAILED_EXIT_CODE)
}
