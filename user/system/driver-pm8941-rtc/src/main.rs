//! Драйвер часов реального времени PMIC Qualcomm `qcom,pm8941-rtc`. Предоставляет протокол
//! `Rtc`: читает 32-битный счётчик секунд часов и ставит их будильник с ожиданием его
//! прерывания.
//!
//! Раскладка регистров соответствует таблице `pm8941_regs` драйвера Linux
//! `drivers/rtc/rtc-pm8xxx.c`.

#![no_std]
#![no_main]

runtime::process_heap!();

extern crate alloc;

use alloc::boxed::Box;
use core::{panic::PanicInfo, sync::atomic::AtomicU64};

use component_startup::{
    Requirements as _, SessionError, component_manifest, mint_session, serve_export,
};
use ipc::{
    CallError, Listener, RecvError,
    wire::{IpcError, MESSAGE_INLINE_MAX},
};
use kernel_objects::{Port, Session, SessionTransport, Signal};
use rtc_abi::rtc;
use runtime::{
    Mutex, PortTransport, Priority, Timeout, log_fmt, log_line, process_exit, spawn_owned,
    start_component,
};
use spmi_abi::{DeviceError, SpmiError, spmi, spmi_device};
use syscall::{Handle, SIGNALED, kind};

const LOG_TAG: &str = "pm8941-rtc";

const COMPATIBLE: &str = "qcom,pm8941-rtc";

const RTC_CONTROL: u16 = 0x6046;
const COUNT: u16 = 0x6048;
const ALARM_VALUE: u16 = 0x6140;
const ALARM_CONTROL: u16 = 0x6146;
const ALARM_CONTROL_2: u16 = 0x6148;

const RTC_ENABLE: u64 = 1 << 7;
const ALARM_ENABLE: u64 = 1 << 7;
const ALARM_CLEAR: u64 = 1 << 0;

const COUNT_BYTES: u8 = 4;
const LOW_BYTE: u64 = 0xFF;

const ALARM_INTERRUPT: u16 = 0;
const ALARM_SECONDS: u64 = 2;
const ALARM_BUDGET: Timeout = Timeout::from_secs(10);

const BUS_CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const EXPORT_THREAD_STACK_SIZE: u64 = 0x10000;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    Bus(CallError<SpmiError>),
    Device(CallError<DeviceError>),
    Silent,
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

        // Отброшенный кадр порт не закрывает, а закрытие порта терминально.
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
        let compatible = ipc::wire::Str::new(COMPATIBLE)
            .ok_or(Failure::Bus(CallError::Ipc(IpcError::InvalidValue)))?;
        let device = bus.open(compatible).map_err(Failure::Bus)?;

        let clock = Self {
            device: Session::<spmi_device::Protocol>::from_session(device)
                .client()
                .with_wait(BUS_CALL_TIMEOUT),
            alarm: Mutex::new(None),
        };

        clock.update(RTC_CONTROL, RTC_ENABLE, 0)?;
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
        self.update(ALARM_CONTROL, 0, ALARM_ENABLE)?;
        self.device.write(ALARM_VALUE, COUNT_BYTES, target)?;
        self.update(ALARM_CONTROL, ALARM_ENABLE, 0)?;

        signal
            .wait(SIGNALED, ALARM_BUDGET)
            .map_err(|_| Failure::Silent)?;

        self.update(ALARM_CONTROL, 0, ALARM_ENABLE)?;
        self.update(ALARM_CONTROL_2, 0, ALARM_CLEAR)?;
        self.device.ack(ALARM_INTERRUPT)?;

        self.count()
    }

    fn update(&self, address: u16, set: u64, clear: u64) -> Result<(), Failure> {
        let value = self.device.read(address, 1)?;
        let updated = (value | set) & !clear;

        if updated != value {
            self.device.write(address, 1, updated)?;
        }

        Ok(())
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
    process_exit(1)
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    start_component(parent, LOG_TAG);
    run(parent);
    process_exit(1)
}

fn run(parent: Handle<kind::Port>) {
    let requirements = RtcStartup::from_parent(parent);
    let bus = match requirements.spmi.open() {
        Ok(bus) => bus.client().with_wait(BUS_CALL_TIMEOUT),
        Err(error) => return log_fmt(format_args!("{error}")),
    };

    let Ok(service) = Port::create() else {
        return log_line("service port refused");
    };

    let shared: &'static Shared = Box::leak(Box::new(Shared {
        service,
        clock: Mutex::new(None),
    }));

    // Поток экспорта работает до конца процесса и держит сессию `Spmi`.
    let exporter = requirements.parent;
    let export = spawn_owned(
        move || {
            let next_badge = AtomicU64::new(0);
            let failure =
                serve_export::<rtc::Protocol>(&exporter, || shared.open(&bus, &next_badge));
            log_fmt(format_args!("export stopped: {failure:?}"));
            process_exit(1)
        },
        EXPORT_THREAD_STACK_SIZE,
        Priority::new(1),
    );

    if export.is_err() {
        return log_line("export thread refused");
    }

    shared.serve();
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    process_exit(1)
}
