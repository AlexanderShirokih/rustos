//! Ресурсы контроллеров QUP от поставщиков `Pins` и `Clocks` и сессии `Platform`.

use clocks_abi::{ClocksError, clocks};
use component_startup::{AnyOf, Providers, StartupError};
use executor::PortChannel;
use i2c_qup::{Failure, Line, Resources};
use ipc::{CallError, Protocol};
use kernel_objects::{IrqLine, MmioMapping, Session, SessionTransport};
use mmio::Registers;
use pins_abi::{PinsError, controllers, device_pins, pins};
use platform_abi::{
    CONTROLLER_KEY, Description, DescriptionError, DeviceId, Node, PlatformError, WindowRefusal,
    platform,
};
use runtime::{Timeout, log_fmt};
use syscall::{SIGNALED, WaitItem};

const CORE_CLOCK: &str = "core";
const CLOCKS: &str = "clocks";
const CLOCK_CELLS: &str = "#clock-cells";
const WINDOW_RESOURCE: u16 = 0;
const LINE_RESOURCE: u16 = 0;
const INTERRUPT_TIMEOUT: Timeout = Timeout::from_secs(2);

type Channel = PortChannel<SessionTransport>;

pub(crate) struct Supplies {
    platform: platform::Client<Channel>,
    pins: Providers<pins::Protocol>,
    clocks: Providers<clocks::Protocol>,
}

pub(crate) struct ControllerWindow {
    mapping: MmioMapping,
    _pins: pins::Client<Channel>,
    _device: device_pins::Client<Channel>,
    _clocks: clocks::Client<Channel>,
}

pub(crate) struct Interrupt {
    line: IrqLine,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Supply {
    State,
    Controllers(usize),
    NoController,
    NoClocks,
    Description(DescriptionError),
    NoProvider,
    Startup(StartupError),
    Pins(CallError<PinsError>),
    Clocks(CallError<ClocksError>),
    Window(WindowRefusal),
    Line(CallError<PlatformError>),
}

impl Supplies {
    pub(crate) fn new(
        platform: platform::Client<Channel>,
        pins: Providers<pins::Protocol>,
        clocks: Providers<clocks::Protocol>,
    ) -> Self {
        Self {
            platform,
            pins,
            clocks,
        }
    }
}

impl Registers<()> for ControllerWindow {
    fn size(&self, (): ()) -> usize {
        Registers::size(&self.mapping, ())
    }

    fn load(&self, (): (), offset: usize) -> u32 {
        self.mapping.load((), offset)
    }

    fn store(&self, (): (), offset: usize, value: u32) {
        self.mapping.store((), offset, value);
    }
}

impl Line for Interrupt {
    async fn wait(&self) -> bool {
        executor::timeout(
            INTERRUPT_TIMEOUT,
            executor::wait(WaitItem::new(self.line.as_raw(), SIGNALED)),
        )
        .await
        .is_some_and(|outcome| outcome.is_ok())
    }

    fn ack(&self) {
        let _ = self.line.ack();
    }

    fn clear(&self) {
        // Сброс контроллера гасит источник уровневой линии до снятия защёлки.
        if self.line.wait(Timeout::POLL).is_ok() {
            let _ = self.line.ack();
        }
    }
}

impl Resources for Supplies {
    type Window = ControllerWindow;
    type Line = Interrupt;
    type Error = Supply;

    async fn enable(
        &self,
        description: &Description<'_>,
        controller: &DeviceId,
        core_hz: u32,
    ) -> Result<ControllerWindow, Supply> {
        let states = controllers(description, controller.as_str()).ok_or(Supply::State)?;
        let [pins_controller] = states.as_slice() else {
            return Err(Supply::Controllers(states.len()));
        };

        let pins = PortChannel::client(provider(&self.pins, pins_controller).await?);
        let device = pins.open(controller.clone()).await.map_err(Supply::Pins)?;
        let device = PortChannel::client(Session::<device_pins::Protocol>::from_session(device));

        let clocks_controller = description
            .node(controller)
            .and_then(|node| node.specifiers(CLOCKS, CLOCK_CELLS))
            .and_then(|mut records| records.next())
            .ok_or(Supply::NoClocks)?
            .map_err(Supply::Description)?;
        let clocks_controller = description
            .find(clocks_controller.node.as_str())
            .ok_or(Supply::NoController)?;
        let clocks = PortChannel::client(provider(&self.clocks, &clocks_controller).await?);
        clocks
            .enable(controller.clone())
            .await
            .map_err(Supply::Clocks)?;

        clocks
            .set_rate(controller.clone(), CORE_CLOCK, u64::from(core_hz))
            .await
            .map_err(Supply::Clocks)?;

        let mapping = self
            .platform
            .map_mmio(controller, WINDOW_RESOURCE)
            .await
            .map_err(Supply::Window)?;

        Ok(ControllerWindow {
            mapping,
            _pins: pins,
            _device: device,
            _clocks: clocks,
        })
    }

    async fn line(&self, controller: &DeviceId) -> Result<Interrupt, Supply> {
        let line = self
            .platform
            .irq(controller.clone(), LINE_RESOURCE)
            .await
            .map(IrqLine::from)
            .map_err(Supply::Line)?;

        Ok(Interrupt { line })
    }

    fn refused(&self, controller: &DeviceId, failure: &Failure<Supply>) {
        log_fmt(format_args!(
            "{} not prepared: {failure:?}",
            controller.as_str()
        ));
    }
}

async fn provider<P: Protocol>(
    providers: &Providers<P>,
    controller: &Node<'_>,
) -> Result<Session<P>, Supply> {
    let compatible = controller
        .strings("compatible")
        .map_err(Supply::Description)?
        .ok_or(Supply::NoController)?;

    providers
        .open_first(AnyOf::new(CONTROLLER_KEY, compatible))
        .await
        .map_err(Supply::Startup)?
        .ok_or(Supply::NoProvider)
}
