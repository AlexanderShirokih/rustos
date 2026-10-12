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
use core::fmt;

use component_startup::{
    AnyOf, Providers, SessionError, SessionEvent, SessionPort, StartupError, component_manifest,
    serve_export,
};
use ipc::CallError;
use kernel_objects::{Port, Session, SessionTransport, Signal};
use platform_abi::{CONTROLLER_KEY, DescriptionError, MappedDescription, PlatformError, platform};
use rtc_abi::rtc;
use runtime::{FAILED_EXIT_CODE, Mutex, Timeout, exit, log_fmt, spawn_bound};
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
        requirements: {
            platform: session(platform::Protocol),
            spmi: all(spmi::Protocol),
        },
        features: ["qcom,pm8941-rtc"],
        exports: [rtc::Protocol],
    }
}

struct Shared {
    sessions: SessionPort,
    clock: Mutex<Option<&'static Clock>>,
}

struct Clock {
    _bus: spmi::Client<SessionTransport>,
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
    Platform(CallError<PlatformError>),
    Mapping(kernel_objects::Error),
    Description(DescriptionError),
    NoNode,
    NoController,
    NoBus,
    Startup(StartupError),
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
        platform: &platform::Client<SessionTransport>,
        spmi: &Providers<spmi::Protocol>,
    ) -> Result<Port, SessionError> {
        let mut clock = self.clock.lock();

        if clock.is_none() {
            let bound = Clock::bind(platform, spmi).map_err(|failure| {
                log_fmt(format_args!("{COMPATIBLE} not bound: {failure:?}"));
                SessionError::Refused
            })?;

            log_fmt(format_args!("{COMPATIBLE} bound"));
            *clock = Some(Box::leak(Box::new(bound)));
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
                let clock = *self.clock.lock();
                if let Some(mut clock) = clock {
                    let _ = rtc::dispatch(&mut clock, frame);
                }
            }
        }
    }
}

impl Clock {
    fn bind(
        platform: &platform::Client<SessionTransport>,
        spmi: &Providers<spmi::Protocol>,
    ) -> Result<Self, Failure> {
        let description = platform.description().map_err(Failure::Platform)?;
        let mapped = MappedDescription::new(description)
            .map_err(|refused| Failure::Mapping(refused.error))?;
        let description = mapped.description().map_err(Failure::Description)?;

        let id = description
            .compatible_devices(COMPATIBLE)
            .next()
            .map(|device| device.id().clone())
            .ok_or(Failure::NoNode)?;
        let controller = spmi_abi::controller(&description, &id).ok_or(Failure::NoController)?;
        let compatible = controller
            .strings("compatible")
            .map_err(Failure::Description)?
            .ok_or(Failure::NoController)?;

        let found = spmi
            .all(AnyOf::new(CONTROLLER_KEY, compatible))
            .map_err(Failure::Startup)?
            .next()
            .ok_or(Failure::NoBus)?
            .map_err(Failure::Startup)?;
        let bus = spmi
            .open(&found)
            .map_err(Failure::Startup)?
            .client()
            .with_wait(BUS_CALL_TIMEOUT);
        let device = bus.open(id.clone()).map_err(Failure::Bus)?;

        let clock = Self {
            _bus: bus,
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
    let platform = requirements
        .platform
        .open()
        .map_err(Stopped::Startup)?
        .client()
        .with_wait(BUS_CALL_TIMEOUT);
    let sessions = SessionPort::create().map_err(|_| Stopped::ServicePort)?;

    let shared: &'static Shared = Box::leak(Box::new(Shared {
        sessions,
        clock: Mutex::new(None),
    }));

    // Поток экспорта работает до конца процесса и держит сессию `Platform`.
    let exporter = requirements.parent;
    let _ = spawn_bound(move || {
        serve_export::<rtc::Protocol>(exporter, |_| shared.open(&platform, &requirements.spmi))
    })
    .map_err(|_| Stopped::ExportThread)?;

    shared.serve();
    Err(Stopped::ServicePortClosed)
}
