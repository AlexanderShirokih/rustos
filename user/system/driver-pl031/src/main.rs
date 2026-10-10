//! Драйвер часов реального времени ARM PrimeCell PL031. Предоставляет протокол `Rtc`: читает
//! текущее время устройства и ставит его будильник с ожиданием прерывания срабатывания.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use core::sync::atomic::AtomicU64;

use component_startup::{SessionError, component_manifest, mint_session, serve_export};
use io::mmio::Reg;
use ipc::{Listener, RecvError, wire::MESSAGE_INLINE_MAX};
use kernel_objects::{
    IrqLine, MemoryRegion, MmioMapping, MmioWindow, Port, ReadOnlyMapping, SessionTransport,
};
use platform_abi::{Description, platform};
use rtc_abi::rtc;
use runtime::{
    FAILED_EXIT_CODE, Mutex, PortTransport, Timeout, exit, log_fmt, log_line, spawn_owned,
};

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
    service: Port,
    device: Mutex<Option<&'static Pl031>>,
}

struct Pl031 {
    registers: MmioMapping,
    line: IrqLine,
}

impl Shared {
    fn open(
        &self,
        platform: &platform::Client<SessionTransport>,
        next_badge: &AtomicU64,
    ) -> core::result::Result<Port, SessionError> {
        let mut device = self.device.lock();

        if device.is_none() {
            let bound = Pl031::bind(platform).map_err(|error| {
                log_fmt(format_args!("{COMPATIBLE} not bound: {error:?}"));
                SessionError::Refused
            })?;
            log_fmt(format_args!("{COMPATIBLE} bound"));
            *device = Some(Box::leak(Box::new(bound)));
        }

        mint_session(&self.service, next_badge)
            .map(|minted| minted.port)
            .map_err(|_| SessionError::NoSpace)
    }

    fn serve(&self) {
        let transport = PortTransport::new(self.service.borrow());
        let mut bytes = [0u8; MESSAGE_INLINE_MAX];
        loop {
            match transport.recv(&mut bytes, Timeout::INFINITE) {
                Ok(frame) => {
                    let device = *self.device.lock();
                    if let Some(mut device) = device {
                        let _ = rtc::dispatch(&mut device, frame);
                    }
                }
                Err(RecvError::SenderGone(_) | RecvError::Discarded | RecvError::Timeout) => {}
                Err(RecvError::Closed) => break,
            }
        }
    }
}

impl Pl031 {
    fn bind(platform: &platform::Client<SessionTransport>) -> Result<Self> {
        let description = platform.description()?;
        let mapping = ReadOnlyMapping::new(MemoryRegion::from(description.region))
            .map_err(kernel_objects::Error::from)?;
        let id = Description::new(mapping.as_bytes(), description.offset)?
            .compatible_devices(COMPATIBLE)
            .next()
            .map(|device| device.id().clone())
            .ok_or(Error::NoDevice)?;
        let line = IrqLine::from(platform.irq(id.as_str(), 0)?);
        let window = platform.mmio(id.as_str(), 0)?;
        let window = MmioWindow {
            region: MemoryRegion::from(window.region),
            offset: window.offset,
            size: window.size,
        };
        Ok(Self {
            registers: MmioMapping::new(window).map_err(kernel_objects::Error::from)?,
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
    let shared: &'static Shared = Box::leak(Box::new(Shared {
        service: Port::create()?,
        device: Mutex::new(None),
    }));
    // Поток экспорта работает до конца процесса и держит сессию `Platform`.
    let exporter = requirements.parent;
    spawn_owned(move || {
        let next_badge = AtomicU64::new(0);
        let failure =
            serve_export::<rtc::Protocol>(&exporter, |_| shared.open(&platform, &next_badge));
        log_fmt(format_args!("export stopped: {failure:?}"));
        exit(FAILED_EXIT_CODE)
    })
    .map_err(|_| Error::Runtime)?;

    shared.serve();
    Err(Error::Ipc)
}
