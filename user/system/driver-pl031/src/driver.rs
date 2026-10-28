use alloc::rc::Rc;
use core::cell::RefCell;

use component_startup::{
    AsyncStartup, Requirements as _, SessionError, SessionPort, component_manifest, serve_component,
};
use executor::{Executor, Mutex, PortChannel};
use io::mmio::Reg;
use kernel_objects::{IrqLine, MmioMapping, SessionTransport};
use platform_abi::{MappedDescription, platform};
use rtc_abi::rtc;
use runtime::{FAILED_EXIT_CODE, Timeout, exit, log_fmt, log_line};
use syscall::{Handle, SIGNALED, WaitItem, kind};

use crate::error::{Error, Result};

type Channel = PortChannel<SessionTransport>;

const RTC_DATA: Reg<u32> = Reg::new(0x00);
const RTC_MATCH: Reg<u32> = Reg::new(0x04);
const RTC_INTERRUPT_MASK: Reg<u32> = Reg::new(0x10);
const RTC_INTERRUPT_CLEAR: Reg<u32> = Reg::new(0x1C);

const ALARM_TICKS: u32 = 2;

const ALARM_BUDGET: Timeout = Timeout::from_secs(10);

const COMPATIBLE: &str = "arm,pl031";

component_manifest! {
    struct RtcStartup {
        requirements: { platform: session(platform::Protocol) },
        features: ["arm,pl031"],
        exports: [rtc::Protocol],
    }
}

struct Pl031 {
    registers: MmioMapping,
    line: IrqLine,
    alarm: Mutex<()>,
}

struct AlarmReset<'a>(&'a Pl031);

impl Pl031 {
    async fn bind(platform: &platform::Client<Channel>) -> Result<Self> {
        let region = platform.description().await?;
        let mapped = MappedDescription::new(region).map_err(kernel_objects::Error::from)?;

        let id = mapped
            .description()?
            .compatible_devices(COMPATIBLE)
            .next()
            .map(|device| device.id().clone())
            .ok_or(Error::NoDevice)?;

        let line = IrqLine::from(platform.irq(id.clone(), 0).await?);
        let registers = platform.map_mmio(&id, 0).await?;

        Ok(Self {
            registers,
            line,
            alarm: Mutex::new(()),
        })
    }

    fn arm(&self) {
        // Будильник ставится на ALARM_TICKS тиков вперёд. Запас покрывает запись
        // регистра между чтением времени и разрешением прерывания.
        let device = self.registers.mmio();
        let now = device.read_reg(RTC_DATA);
        device.write_reg(RTC_MATCH, now.wrapping_add(ALARM_TICKS));
        device.write_reg(RTC_INTERRUPT_MASK, 1);
    }

    fn acknowledge(&self) -> Result<()> {
        self.registers.mmio().write_reg(RTC_INTERRUPT_CLEAR, 1);
        self.line.ack()?;
        Ok(())
    }
}

#[allow(clippy::unused_async_trait_impl)]
impl<F: ipc::ReceivedResources> rtc::Service<F> for Pl031 {
    async fn now(&self, _frame: &F) -> u64 {
        u64::from(self.registers.mmio().read_reg(RTC_DATA))
    }

    async fn tick(&self, _frame: &F) -> u64 {
        let _alarm = self.alarm.lock().await;
        let _reset = AlarmReset(self);
        self.arm();

        let line = WaitItem::new(self.line.as_raw(), SIGNALED);
        let completed = executor::timeout(ALARM_BUDGET, executor::wait(line)).await;
        if !matches!(completed, Some(Ok(()))) || self.acknowledge().is_err() {
            log_line("alarm line silent");
            exit(FAILED_EXIT_CODE);
        }

        u64::from(self.registers.mmio().read_reg(RTC_DATA))
    }
}

impl Drop for AlarmReset<'_> {
    fn drop(&mut self) {
        let device = self.0.registers.mmio();
        device.write_reg(RTC_INTERRUPT_MASK, 0);
        device.write_reg(RTC_INTERRUPT_CLEAR, 1);

        if self.0.line.wait(Timeout::POLL).is_ok() {
            let _ = self.0.line.ack();
        }
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<()> {
    let requirements = RtcStartup::from_parent(parent);
    let platform_session = requirements.platform.open()?;

    let executor = Rc::new(Executor::new()?);

    let platform = PortChannel::client(platform_session);

    let device = Rc::new(RefCell::new(None::<Rc<Pl031>>));

    let sessions = Rc::new(SessionPort::create()?);

    let export = sessions.clone();
    let bound = device.clone();

    let served = serve_component::<rtc::Protocol, _>(
        &executor,
        AsyncStartup::new(PortChannel::new(parent)),
        sessions,
        move |frame| {
            let device = device.borrow().clone();
            let device = device.ok_or(ipc::wire::IpcError::PeerClosed)?;
            rtc::serve(&device, frame)
        },
        async move |_| {
            if bound.borrow().is_none() {
                let device = Pl031::bind(&platform).await.map_err(|error| {
                    log_fmt(format_args!("{COMPATIBLE} not bound: {error:?}"));
                    SessionError::Refused
                })?;

                *bound.borrow_mut() = Some(Rc::new(device));
                log_fmt(format_args!("{COMPATIBLE} bound"));
            }

            export.open_session()
        },
        |_| {},
    );

    let _ = executor.block_on(served);
    Err(Error::Ipc)
}
