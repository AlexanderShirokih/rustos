//! Ресурсы контроллеров QUP от поставщиков `Pins` и `Clocks` и сессии `Platform`.

use alloc::vec::Vec;

use clocks_abi::{ClocksError, clocks};
use component_startup::{AnyOf, Providers, StartupError};
use i2c_qup::{Failure, Resources};
use ipc::{CallError, Protocol};
use kernel_objects::{IrqLine, MmioMapping, Session, SessionTransport};
use pins_abi::{PinsError, controllers, device_pins, pins};
use platform_abi::{
    CONTROLLER_KEY, Description, DescriptionError, DeviceId, Node, PlatformError, WindowRefusal,
    platform,
};
use runtime::log_fmt;

const CORE_CLOCK: &str = "core";
const CLOCKS: &str = "clocks";
const CLOCK_CELLS: &str = "#clock-cells";
const WINDOW_RESOURCE: u16 = 0;
const LINE_RESOURCE: u16 = 0;

pub(crate) struct Supplies<'p> {
    platform: &'p platform::Client<SessionTransport>,
    pins: Providers<pins::Protocol>,
    clocks: Providers<clocks::Protocol>,
    held_pins: Vec<(
        pins::Client<SessionTransport>,
        device_pins::Client<SessionTransport>,
    )>,
    held_clocks: Vec<clocks::Client<SessionTransport>>,
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

impl<'p> Supplies<'p> {
    pub(crate) fn new(
        platform: &'p platform::Client<SessionTransport>,
        pins: Providers<pins::Protocol>,
        clocks: Providers<clocks::Protocol>,
    ) -> Self {
        Self {
            platform,
            pins,
            clocks,
            held_pins: Vec::new(),
            held_clocks: Vec::new(),
        }
    }
}

impl Resources for Supplies<'_> {
    type Window = MmioMapping;
    type Line = IrqLine;
    type Error = Supply;

    fn enable(
        &mut self,
        description: &Description<'_>,
        controller: &DeviceId,
        core_hz: u32,
    ) -> Result<MmioMapping, Supply> {
        let states = controllers(description, controller.as_str()).ok_or(Supply::State)?;
        let [pins_controller] = states.as_slice() else {
            return Err(Supply::Controllers(states.len()));
        };
        let pins = provider(&self.pins, pins_controller)?.client();
        let device = pins.open(controller.clone()).map_err(Supply::Pins)?;
        let device = Session::<device_pins::Protocol>::from_session(device).client();
        self.held_pins.push((pins, device));

        let clocks_controller = description
            .node(controller)
            .and_then(|node| node.specifiers(CLOCKS, CLOCK_CELLS))
            .and_then(|mut records| records.next())
            .ok_or(Supply::NoClocks)?
            .map_err(Supply::Description)?;
        let clocks_controller = description
            .find(clocks_controller.node.as_str())
            .ok_or(Supply::NoController)?;
        let clocks = provider(&self.clocks, &clocks_controller)?.client();
        clocks.enable(controller.clone()).map_err(Supply::Clocks)?;
        clocks
            .set_rate(controller.clone(), CORE_CLOCK, u64::from(core_hz))
            .map_err(Supply::Clocks)?;
        self.held_clocks.push(clocks);

        self.platform
            .map_mmio(controller, WINDOW_RESOURCE)
            .map_err(Supply::Window)
    }

    fn line(&mut self, controller: &DeviceId) -> Result<IrqLine, Supply> {
        self.platform
            .irq(controller.clone(), LINE_RESOURCE)
            .map(IrqLine::from)
            .map_err(Supply::Line)
    }

    fn refused(&mut self, controller: &DeviceId, failure: &Failure<Supply>) {
        log_fmt(format_args!(
            "{} not prepared: {failure:?}",
            controller.as_str()
        ));
    }
}

fn provider<P: Protocol>(
    providers: &Providers<P>,
    controller: &Node<'_>,
) -> Result<Session<P>, Supply> {
    let compatible = controller
        .strings("compatible")
        .map_err(Supply::Description)?
        .ok_or(Supply::NoController)?;
    let found = providers
        .all(AnyOf::new(CONTROLLER_KEY, compatible))
        .map_err(Supply::Startup)?
        .next()
        .ok_or(Supply::NoProvider)?
        .map_err(Supply::Startup)?;
    providers.open(&found).map_err(Supply::Startup)
}
