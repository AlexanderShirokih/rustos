//! Драйвер PL031 RTC: компонент композиции, получающий линию прерывания и
//! MMIO-окно устройства слотами стартовой транзакции. Описание платформы
//! драйверу не выдаётся.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

use core::panic::PanicInfo;

use component_abi::receive_startup_transaction;
use io::mmio::{Mmio, Reg};
use ipc::wire::IpcError;
use rtc_abi::{RtcService, dispatch_rtc};
use runtime::{
    IrqLine, MemoryRegion, OwnedHandle, PortTransport, Timeout, UserMemFlags, install_log,
    log_line, thread_exit,
};
use syscall::Handle;

mod error;

use error::{Error, Result};

const LOG_TAG: &str = "rtc";

const RTC_DATA: Reg<u32> = Reg::new(0x00);
const RTC_MATCH: Reg<u32> = Reg::new(0x04);
const RTC_INTERRUPT_MASK: Reg<u32> = Reg::new(0x10);
const RTC_INTERRUPT_CLEAR: Reg<u32> = Reg::new(0x1C);

const ALARM_TICKS: u32 = 2;

const ALARM_BUDGET: Timeout = Timeout::from_secs(10);

const SLOT_LINE: u16 = 0;
const SLOT_WINDOW: u16 = 1;

const ALARM_LOST_EXIT_CODE: u32 = 5;

struct Pl031 {
    device: Mmio,
    line: IrqLine,
}

impl Pl031 {
    fn arm(&self) {
        // Будильник ставится через ALARM_TICKS тиков: запас покрывает запись
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

impl RtcService for Pl031 {
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

/// `startup` приходит в x0 как HandleId одноразового startup-порта.
#[unsafe(no_mangle)]
pub extern "C" fn _start(startup: Handle) -> ! {
    thread_exit(u64::from(run(startup).is_err()))
}

fn run(startup: Handle) -> Result<()> {
    let startup = OwnedHandle::from_handle(startup);
    let started = receive_startup_transaction(&PortTransport::server(startup.borrow()))?
        .ok_or(Error::Start)?;
    drop(startup);

    install_log(OwnedHandle::adopt(started.log), LOG_TAG);
    // Клиентская сторона control-порта остаётся у supervisor: серверная живёт
    // до конца процесса вместе с ней.
    let _control = OwnedHandle::adopt(started.control);
    let service = OwnedHandle::adopt(started.service);

    let line = IrqLine::from_handle(OwnedHandle::adopt(
        started.slot(SLOT_LINE).ok_or(Error::Start)?,
    ));
    let window = MemoryRegion::from_handle(OwnedHandle::adopt(
        started.slot(SLOT_WINDOW).ok_or(Error::Start)?,
    ));
    let mapping = window.map_full(UserMemFlags::ReadWrite)?;
    let mut driver = Pl031 {
        device: mapping.mmio(),
        line,
    };

    log_line("started");
    serve(&service, &mut driver);
    Ok(())
}

fn serve(service: &OwnedHandle, driver: &mut Pl031) {
    let transport = PortTransport::server(service.borrow());
    // Ни один исход кадра не несёт: драйвер не хранит сессий, а уход значка и
    // покадровое исчерпание порт не закрывают. Прочий отказ терминален.
    while matches!(
        dispatch_rtc(driver, &transport),
        Ok(()) | Err(IpcError::SenderGone | IpcError::ResourceExhausted | IpcError::Timeout)
    ) {}
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
