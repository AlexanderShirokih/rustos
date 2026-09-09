//! Драйвер PL031 RTC: компонент композиции, получающий линию прерывания и
//! MMIO-окно устройства слотами стартовой транзакции. Описание платформы
//! драйверу не выдаётся.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_startup::{ReceivedStartup, component_manifest};
use io::mmio::{Mmio, Reg};
use ipc::wire::IpcError;
use kernel_objects::{IrqLine, Port};
use rtc_abi::rtc;
use runtime::{PortTransport, Timeout, UserMemFlags, install_log, log_line, thread_exit};
use syscall::{Handle, kind};

mod error;

use error::Result;

const LOG_TAG: &str = "rtc";

const RTC_DATA: Reg<u32> = Reg::new(0x00);
const RTC_MATCH: Reg<u32> = Reg::new(0x04);
const RTC_INTERRUPT_MASK: Reg<u32> = Reg::new(0x10);
const RTC_INTERRUPT_CLEAR: Reg<u32> = Reg::new(0x1C);

const ALARM_TICKS: u32 = 2;

const ALARM_BUDGET: Timeout = Timeout::from_secs(10);

const ALARM_LOST_EXIT_CODE: u32 = 5;

component_manifest! {
    struct RtcStartup {
        exports: { service: "Rtc" },
        slots: {
            line: irq(compatible = ["arm,pl031"]),
            window: mmio(compatible = ["arm,pl031"]),
        },
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

impl rtc::Service for Pl031 {
    fn now(&mut self) -> u64 {
        u64::from(self.device.read_reg(RTC_DATA))
    }

    fn tick(&mut self) -> u64 {
        self.arm();
        if self.line.wait(ALARM_BUDGET).is_err() || self.acknowledge().is_err() {
            log_line("alarm line silent");
            thread_exit(u64::from(ALARM_LOST_EXIT_CODE));
        }
        u64::from(self.device.read_reg(RTC_DATA))
    }
}

/// `startup` - серверная сторона startup-порта, аргумент точки входа.
#[unsafe(no_mangle)]
pub extern "C" fn _start(startup: Handle<kind::Port>) -> ! {
    thread_exit(u64::from(run(startup).is_err()))
}

fn run(startup: Handle<kind::Port>) -> Result<()> {
    let ReceivedStartup { log, resources } = RtcStartup::receive(Port::from_handle(startup))?;
    install_log(log.into_raw(), LOG_TAG);
    // Клиентскую сторону startup-порта supervisor держит всю incarnation. Серверная
    // остаётся открытой до конца процесса вместе с ней.
    let _startup = resources.startup;
    let service = resources.service;

    let line = resources.line;
    let window = resources.window;
    let mapping = window.map_full(UserMemFlags::ReadWrite)?;
    let mut driver = Pl031 {
        device: mapping.mmio(),
        line,
    };

    log_line("started");
    serve(&service, &mut driver);
    Ok(())
}

fn serve(service: &Port, driver: &mut Pl031) {
    let transport = PortTransport::server(service.borrow());
    // Драйвер не хранит сессий. Уход значка и покадровое исчерпание порт не закрывают, а
    // прочий отказ терминален.
    while matches!(
        rtc::dispatch(driver, &transport),
        Ok(()) | Err(IpcError::SenderGone | IpcError::ResourceExhausted | IpcError::Timeout)
    ) {}
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
