//! Драйвер часов реального времени ARM PrimeCell PL031. Предоставляет протокол `Rtc`: читает
//! текущее время устройства и ставит его будильник с ожиданием прерывания срабатывания.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

extern crate alloc;

use alloc::boxed::Box;
use core::{convert::Infallible, panic::PanicInfo, sync::atomic::AtomicU64};

use component_startup::{
    Requirements as _, SessionError, component_manifest, mint_session, serve_export,
};
use io::mmio::Reg;
use ipc::{Listener, RecvError, wire::MESSAGE_INLINE_MAX};
use kernel_objects::{IrqLine, MemoryRegion, MmioMapping, MmioWindow, Port, SessionTransport};
use platform_abi::{Description, DeviceId, platform};
use rtc_abi::rtc;
use runtime::{
    Mutex, PortTransport, Priority, Timeout, log_fmt, log_line, process_exit, spawn_owned,
    start_component,
};
use syscall::{Handle, kind};

mod error;

use error::{Error, Result};

const LOG_TAG: &str = "rtc";

const RTC_DATA: Reg<u32> = Reg::new(0x00);
const RTC_MATCH: Reg<u32> = Reg::new(0x04);
const RTC_INTERRUPT_MASK: Reg<u32> = Reg::new(0x10);
const RTC_INTERRUPT_CLEAR: Reg<u32> = Reg::new(0x1C);

const ALARM_TICKS: u32 = 2;

const ALARM_BUDGET: Timeout = Timeout::from_secs(10);

const EXPORT_THREAD_STACK_SIZE: u64 = 0x10000;

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
    id: DeviceId,
    registers: MmioMapping,
    _window: MmioWindow,
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
            log_fmt(format_args!("{} bound", bound.id.as_str()));
            *device = Some(Box::leak(Box::new(bound)));
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
        let description = platform.description()??;
        // Регион описания нужен только на время поиска узла.
        let region = MemoryRegion::from(description.region);
        let mapping = region.map_full_read_only()?;
        let id = Description::new(mapping.as_bytes(), description.offset)?
            .devices()
            .filter_map(core::result::Result::ok)
            .find(|device| {
                device
                    .compatible()
                    .any(|compatible| compatible == COMPATIBLE)
            })
            .map(|device| device.id().clone())
            .ok_or(Error::NoDevice)?;
        let id = &id;
        let line = IrqLine::from(platform.irq(id.as_wire(), 0)??);
        let window = platform.mmio(id.as_wire(), 0)??;
        let window = MmioWindow {
            region: MemoryRegion::from(window.region),
            offset: window.offset,
            size: window.size,
        };
        // Устройство держит окно до конца процесса, и закрытие окна отзывает выведенные из него
        // страницы.
        Ok(Self {
            id: id.clone(),
            registers: window.map()?,
            _window: window,
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
            process_exit(1);
        }
        u64::from(self.registers.mmio().read_reg(RTC_DATA))
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    let Err(error) = run(parent);
    log_fmt(format_args!("stopped: {error:?}"));
    process_exit(1)
}

fn run(parent: Handle<kind::Port>) -> Result<Infallible> {
    start_component(parent, LOG_TAG);
    let requirements = RtcStartup::request(parent)?;
    let platform = requirements.platform.ok_or(Error::Start)?.client();
    let shared: &'static Shared = Box::leak(Box::new(Shared {
        service: Port::create()?,
        device: Mutex::new(None),
    }));
    // Поток экспорта работает до конца процесса и держит сессию `Platform`.
    let exporter = requirements.parent;
    spawn_owned(
        move || {
            let next_badge = AtomicU64::new(0);
            let failure =
                serve_export::<rtc::Protocol>(&exporter, || shared.open(&platform, &next_badge));
            log_fmt(format_args!("export stopped: {failure:?}"));
            process_exit(1)
        },
        EXPORT_THREAD_STACK_SIZE,
        Priority::new(1),
    )
    .map_err(|_| Error::Runtime)?;

    log_line("started");
    shared.serve();
    Err(Error::Ipc)
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    process_exit(1)
}
