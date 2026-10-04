//! Драйвер PL031 RTC: компонент композиции, который находит устройство в описании платформы и
//! получает его линию прерывания и окно регистров по сессии `Platform`.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

extern crate alloc;

use alloc::boxed::Box;
use core::{convert::Infallible, panic::PanicInfo};

use component_startup::{Requirements as _, component_manifest, serve_export, whole_export};
use io::mmio::{Mmio, Reg};
use ipc::{Listener, RecvError, wire::MESSAGE_INLINE_MAX};
use kernel_objects::{IrqLine, MemoryRegion, MmioWindow, Port, SessionTransport};
use platform_abi::{Description, DeviceId, platform};
use rtc_abi::rtc;
use runtime::{
    PortTransport, Priority, Timeout, log_fmt, log_line, process_exit, spawn_owned, start_component,
};
use syscall::{Handle, kind};

mod error;

use error::{Error, Result};

const LOG_TAG: &str = "rtc";

const COMPATIBLE: &str = "arm,pl031";

const RTC_DATA: Reg<u32> = Reg::new(0x00);
const RTC_MATCH: Reg<u32> = Reg::new(0x04);
const RTC_INTERRUPT_MASK: Reg<u32> = Reg::new(0x10);
const RTC_INTERRUPT_CLEAR: Reg<u32> = Reg::new(0x1C);

const ALARM_TICKS: u32 = 2;

const ALARM_BUDGET: Timeout = Timeout::from_secs(10);

const EXPORT_STACK_SIZE: u64 = 0x4000;

component_manifest! {
    struct RtcStartup {
        requirements: { platform: session(platform::Protocol) },
        exports: [rtc::Protocol],
    }
}

struct Pl031 {
    device: Mmio,
    line: IrqLine,
    _platform: platform::Client<SessionTransport>,
}

impl Pl031 {
    fn arm(&self) {
        // Будильник ставится на ALARM_TICKS тиков вперёд. Запас покрывает запись
        // регистра между чтением времени и разрешением прерывания.
        let now = self.device.read_reg(RTC_DATA);
        self.device
            .write_reg(RTC_MATCH, now.wrapping_add(ALARM_TICKS));
        self.device.write_reg(RTC_INTERRUPT_MASK, 1);
    }

    fn acknowledge(&self) -> Result<()> {
        self.device.write_reg(RTC_INTERRUPT_CLEAR, 1);
        self.line.ack()?;
        Ok(())
    }
}

impl<F: ipc::ReceivedResources> rtc::Service<F> for Pl031 {
    fn now(&mut self, _frame: &F) -> u64 {
        u64::from(self.device.read_reg(RTC_DATA))
    }

    fn tick(&mut self, _frame: &F) -> u64 {
        self.arm();
        if self.line.wait(ALARM_BUDGET).is_err() || self.acknowledge().is_err() {
            log_line("alarm line silent");
            process_exit(1);
        }
        u64::from(self.device.read_reg(RTC_DATA))
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
    let service: &'static Port = Box::leak(Box::new(Port::create()?));
    // Поток экспорта работает до конца процесса.
    let exporter = requirements.parent;
    spawn_owned(
        move || {
            let failure = serve_export::<rtc::Protocol>(&exporter, whole_export(service));
            log_fmt(format_args!("export stopped: {failure:?}"));
            process_exit(1)
        },
        EXPORT_STACK_SIZE,
        Priority::new(1),
    )
    .map_err(|_| Error::Runtime)?;

    let platform = requirements.platform.ok_or(Error::Start)?.client();
    let device = find_device(&platform)?;
    let line = IrqLine::from(platform.irq(device.as_wire(), 0)??);
    let window = platform.mmio(device.as_wire(), 0)??;
    let window = MmioWindow {
        region: MemoryRegion::from(window.region),
        offset: window.offset,
        size: window.size,
    };
    let mapping = window.map()?;
    let mut driver = Pl031 {
        device: mapping.mmio(),
        line,
        _platform: platform,
    };

    log_line("started");
    serve(service, &mut driver);
    Err(Error::Ipc)
}

fn find_device(platform: &platform::Client<SessionTransport>) -> Result<DeviceId> {
    let description = platform.description()??;
    let region = MemoryRegion::from(description.region);
    let mapping = region.map_full_read_only()?;
    let tree = Description::new(mapping.as_bytes(), description.offset)?;

    let mut refusal = None;
    for device in tree.devices() {
        match device {
            Ok(device) if device.compatible().any(|name| name == COMPATIBLE) => {
                return Ok(device.id().clone());
            }
            Ok(_) => {}
            Err(error) => {
                refusal.get_or_insert(error);
            }
        }
    }
    Err(refusal.map_or(Error::NoDevice, Error::Description))
}

fn serve(service: &Port, driver: &mut Pl031) {
    let transport = PortTransport::new(service.borrow());
    let mut bytes = [0u8; MESSAGE_INLINE_MAX];
    // Драйвер не хранит сессий. Уход значка и отброшенный кадр порт не закрывают, а закрытие
    // порта терминально.
    loop {
        match transport.recv(&mut bytes, Timeout::INFINITE) {
            Ok(frame) => {
                let _ = rtc::dispatch(driver, frame);
            }
            Err(RecvError::SenderGone(_) | RecvError::Discarded | RecvError::Timeout) => {}
            Err(RecvError::Closed) => break,
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    process_exit(1)
}
