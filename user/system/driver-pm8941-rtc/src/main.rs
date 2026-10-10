//! Драйвер часов реального времени PMIC Qualcomm `qcom,pm8941-rtc`. Предоставляет протокол
//! `Rtc`: читает 32-битный счётчик секунд часов и ставит их будильник с ожиданием его
//! прерывания.
//!
//! Раскладка регистров соответствует таблице `pm8941_regs` драйвера Linux
//! `drivers/rtc/rtc-pm8xxx.c`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use core::{fmt, sync::atomic::AtomicU64};

use component_startup::{
    SessionError, StartupError, component_manifest, mint_session, serve_export,
};
use ipc::{CallError, Listener, RecvError, wire::MESSAGE_INLINE_MAX};
use kernel_objects::{Port, Session, SessionTransport, Signal};
use rtc_abi::rtc;
use runtime::{FAILED_EXIT_CODE, Mutex, PortTransport, Timeout, exit, log_fmt, spawn_owned};
use spmi_abi::{DeviceError, SpmiError, spmi, spmi_device};
use syscall::SIGNALED;

const COMPATIBLE: &str = "qcom,pm8941-rtc";

const RTC_CONTROL: u16 = 0x6046;
const COUNT: u16 = 0x6048;
const ALARM_VALUE: u16 = 0x6140;
const ALARM_CONTROL: u16 = 0x6146;
const ALARM_CONTROL_2: u16 = 0x6148;

const RTC_ENABLE: u8 = 1 << 7;
const ALARM_ENABLE: u8 = 1 << 7;
const ALARM_CLEAR: u8 = 1 << 0;

const COUNT_BYTES: u8 = 4;
const LOW_BYTE: u64 = 0xFF;

const ALARM_INTERRUPT: u16 = 0;
const ALARM_SECONDS: u64 = 2;
const ALARM_BUDGET: Timeout = Timeout::from_secs(10);

const BUS_CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

component_manifest! {
    struct RtcStartup {
        requirements: { spmi: session(spmi::Protocol) },
        features: ["qcom,pm8941-rtc"],
        exports: [rtc::Protocol],
    }
}

struct Shared {
    service: Port,
    clock: Mutex<Option<&'static Clock>>,
}

struct Clock {
    device: spmi_device::Client<SessionTransport>,
    alarm: Mutex<Option<Signal>>,
}

#[derive(Debug)]
enum Stopped {
    Startup(StartupError),
    ServicePort,
    ExportThread,
    ServicePortClosed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    Bus(CallError<SpmiError>),
    Device(CallError<DeviceError>),
    Silent,
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => error.fmt(formatter),
            Self::ServicePort => formatter.write_str("service port refused"),
            Self::ExportThread => formatter.write_str("export thread refused"),
            Self::ServicePortClosed => formatter.write_str("service port closed"),
        }
    }
}

impl Shared {
    fn open(
        &self,
        bus: &spmi::Client<SessionTransport>,
        next_badge: &AtomicU64,
    ) -> Result<Port, SessionError> {
        let mut clock = self.clock.lock();

        if clock.is_none() {
            let bound = Clock::bind(bus).map_err(|failure| {
                log_fmt(format_args!("{COMPATIBLE} not bound: {failure:?}"));
                SessionError::Refused
            })?;

            log_fmt(format_args!("{COMPATIBLE} bound"));
            *clock = Some(Box::leak(Box::new(bound)));
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
                    let clock = *self.clock.lock();
                    if let Some(mut clock) = clock {
                        let _ = rtc::dispatch(&mut clock, frame);
                    }
                }
                Err(RecvError::SenderGone(_) | RecvError::Discarded | RecvError::Timeout) => {}
                Err(RecvError::Closed) => break,
            }
        }
    }
}

impl Clock {
    fn bind(bus: &spmi::Client<SessionTransport>) -> Result<Self, Failure> {
        let compatible = COMPATIBLE;
        let device = bus.open(compatible).map_err(Failure::Bus)?;

        let clock = Self {
            device: Session::<spmi_device::Protocol>::from_session(device)
                .client()
                .with_wait(BUS_CALL_TIMEOUT),
            alarm: Mutex::new(None),
        };

        clock.device.update(RTC_CONTROL, RTC_ENABLE, RTC_ENABLE)?;
        Ok(clock)
    }

    fn count(&self) -> Result<u64, Failure> {
        let count = self.device.read(COUNT, COUNT_BYTES)?;
        let low = self.device.read(COUNT, 1)?;

        // Младший байт, ставший меньше, перенёсся в старшие между чтениями.
        if low < (count & LOW_BYTE) {
            return Ok(self.device.read(COUNT, COUNT_BYTES)?);
        }

        Ok(count)
    }

    fn alarm(&self) -> Result<u64, Failure> {
        let mut alarm = self.alarm.lock();
        let signal = match alarm.take() {
            Some(signal) => signal,
            None => Signal::from(self.device.irq(ALARM_INTERRUPT)?),
        };
        let signal = alarm.insert(signal);

        let target = (self.count()? + ALARM_SECONDS) & u64::from(u32::MAX);
        self.device.update(ALARM_CONTROL, ALARM_ENABLE, 0)?;
        self.device.write(ALARM_VALUE, COUNT_BYTES, target)?;
        self.device
            .update(ALARM_CONTROL, ALARM_ENABLE, ALARM_ENABLE)?;

        signal
            .wait(SIGNALED, ALARM_BUDGET)
            .map_err(|_| Failure::Silent)?;

        self.device.update(ALARM_CONTROL, ALARM_ENABLE, 0)?;
        self.device.update(ALARM_CONTROL_2, ALARM_CLEAR, 0)?;
        self.device.ack(ALARM_INTERRUPT)?;

        self.count()
    }
}

impl<F: ipc::ReceivedResources> rtc::Service<F> for &Clock {
    fn now(&mut self, _frame: &F) -> u64 {
        self.count()
            .unwrap_or_else(|failure| stop("count", failure))
    }

    fn tick(&mut self, _frame: &F) -> u64 {
        self.alarm()
            .unwrap_or_else(|failure| stop("alarm", failure))
    }
}

impl From<CallError<DeviceError>> for Failure {
    fn from(error: CallError<DeviceError>) -> Self {
        Self::Device(error)
    }
}

fn stop(operation: &str, failure: Failure) -> ! {
    log_fmt(format_args!("{operation} failed: {failure:?}"));
    exit(FAILED_EXIT_CODE)
}

#[program_entry::component]
fn run(requirements: RtcStartup) -> Result<(), Stopped> {
    let bus = requirements
        .spmi
        .open()
        .map_err(Stopped::Startup)?
        .client()
        .with_wait(BUS_CALL_TIMEOUT);
    let service = Port::create().map_err(|_| Stopped::ServicePort)?;

    let shared: &'static Shared = Box::leak(Box::new(Shared {
        service,
        clock: Mutex::new(None),
    }));

    // Поток экспорта работает до конца процесса и держит сессию `Spmi`.
    let exporter = requirements.parent;
    let _ = spawn_owned(move || {
        let next_badge = AtomicU64::new(0);
        let failure = serve_export::<rtc::Protocol>(&exporter, |_| shared.open(&bus, &next_badge));
        log_fmt(format_args!("export stopped: {failure:?}"));
        exit(FAILED_EXIT_CODE)
    })
    .map_err(|_| Stopped::ExportThread)?;

    shared.serve();
    Err(Stopped::ServicePortClosed)
}
