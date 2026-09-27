//! Драйвер PL031 RTC: компонент композиции, получающий линию прерывания и
//! MMIO-окно устройства требованиями своего манифеста. Описание платформы
//! драйверу не выдаётся.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

extern crate alloc;

use alloc::boxed::Box;
use core::{convert::Infallible, panic::PanicInfo};

use component_startup::{component_manifest, serve_export, whole_export};
use io::mmio::{Mmio, Reg};
use ipc::{Listener, RecvError, wire::MESSAGE_INLINE_MAX};
use kernel_objects::{IrqLine, Port};
use rtc_abi::rtc;
use runtime::{
    PortTransport, Priority, Timeout, UserMemFlags, install_log, log_fmt, log_line, process_exit,
    spawn_owned,
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

const FAILED_EXIT_CODE: u64 = 1;
const ALARM_LOST_EXIT_CODE: u64 = 5;
const EXPORT_FAILED_EXIT_CODE: u64 = 6;

const EXPORT_STACK_SIZE: u64 = 0x4000;

component_manifest! {
    struct RtcStartup {
        requirements: {
            line: irq(compatible = ["arm,pl031"]),
            window: mmio(compatible = ["arm,pl031"]),
        },
        exports: [rtc::Protocol],
    }
}

struct Pl031 {
    device: Mmio,
    line: IrqLine,
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
            process_exit(ALARM_LOST_EXIT_CODE);
        }
        u64::from(self.device.read_reg(RTC_DATA))
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    let Err(error) = run(parent);
    log_fmt(format_args!("stopped: {error:?}"));
    process_exit(FAILED_EXIT_CODE)
}

fn run(parent: Handle<kind::Port>) -> Result<Infallible> {
    let started = RtcStartup::start(parent)?;
    if let Some(log) = started.log {
        install_log(log.into_handle().into_raw(), LOG_TAG);
    }
    let service: &'static Port = Box::leak(Box::new(Port::create()?));
    // Поток экспорта работает до конца процесса.
    let exporter = started.requirements.parent;
    spawn_owned(
        move || {
            let failure = serve_export::<rtc::Protocol>(&exporter, whole_export(service));
            log_fmt(format_args!("export stopped: {failure:?}"));
            process_exit(EXPORT_FAILED_EXIT_CODE)
        },
        EXPORT_STACK_SIZE,
        Priority::new(1),
    )
    .map_err(|_| Error::Runtime)?;

    let mapping = started
        .requirements
        .window
        .map_full(UserMemFlags::ReadWrite)?;
    let mut driver = Pl031 {
        device: mapping.mmio(),
        line: started.requirements.line,
    };

    log_line("started");
    serve(service, &mut driver);
    Err(Error::Ipc)
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
    process_exit(FAILED_EXIT_CODE)
}
