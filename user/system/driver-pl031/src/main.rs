//! Драйвер часов реального времени ARM PrimeCell PL031. Предоставляет протокол `Rtc`: читает
//! текущее время устройства и ставит его будильник с ожиданием прерывания срабатывания.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::sync::Arc;

use component_startup::{
    SessionError, SessionEvent, SessionPort, component_manifest, serve_export,
};
use io::mmio::Reg;
use kernel_objects::{IrqLine, MmioMapping, Port, SessionTransport};
use platform_abi::{MappedDescription, platform};
use rtc_abi::rtc;
use runtime::{FAILED_EXIT_CODE, Mutex, Timeout, exit, log_fmt, log_line, spawn_bound};

mod error;

use error::{Error, Result};

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

struct Shared {
    sessions: SessionPort,
    device: Mutex<Option<Arc<Pl031>>>,
}

struct Pl031 {
    registers: MmioMapping,
    line: IrqLine,
}

impl Shared {
    fn open(
        &self,
        platform: &platform::Client<SessionTransport>,
    ) -> core::result::Result<Port, SessionError> {
        let mut device = self.device.lock();

        if device.is_none() {
            let bound = Pl031::bind(platform).map_err(|error| {
                log_fmt(format_args!("{COMPATIBLE} not bound: {error:?}"));
                SessionError::Refused
            })?;
            log_fmt(format_args!("{COMPATIBLE} bound"));
            *device = Some(Arc::new(bound));
        }

        self.sessions
            .create_session()
            .map(|minted| minted.port)
            .map_err(|_| SessionError::NoSpace)
    }

    fn serve(&self) {
        let mut events = self.sessions.events();

        while let Some(event) = events.recv() {
            if let SessionEvent::Frame(frame) = event {
                let device = self.device.lock().clone();
                if let Some(device) = device {
                    let _ = rtc::dispatch(&mut &*device, frame);
                }
            }
        }
    }
}

impl Pl031 {
    fn bind(platform: &platform::Client<SessionTransport>) -> Result<Self> {
        let mapped =
            MappedDescription::new(platform.description()?).map_err(kernel_objects::Error::from)?;
        let id = mapped
            .description()?
            .compatible_devices(COMPATIBLE)
            .next()
            .map(|device| device.id().clone())
            .ok_or(Error::NoDevice)?;
        let line = IrqLine::from(platform.irq(id.clone(), 0)?);
        Ok(Self {
            registers: platform.map_mmio(&id, 0)?,
            line,
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

impl<F: ipc::ReceivedResources> rtc::Service<F> for &Pl031 {
    fn now(&mut self, _frame: &F) -> u64 {
        u64::from(self.registers.mmio().read_reg(RTC_DATA))
    }

    fn tick(&mut self, _frame: &F) -> u64 {
        self.arm();
        if self.line.wait(ALARM_BUDGET).is_err() || self.acknowledge().is_err() {
            log_line("alarm line silent");
            exit(FAILED_EXIT_CODE);
        }
        u64::from(self.registers.mmio().read_reg(RTC_DATA))
    }
}

#[program_entry::component]
fn run(requirements: RtcStartup) -> Result<()> {
    let platform = requirements.platform.open()?.client();
    let shared = Arc::new(Shared {
        sessions: SessionPort::create()?,
        device: Mutex::new(None),
    });

    // Поток экспорта держит сессию `Platform`.
    let exporter = requirements.parent;
    let opener = Arc::clone(&shared);
    spawn_bound(move || serve_export::<rtc::Protocol>(exporter, |_| opener.open(&platform)))
        .map_err(|_| Error::Runtime)?;

    shared.serve();
    Err(Error::Ipc)
}
