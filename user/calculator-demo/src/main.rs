//! Демо композиции: компонент-клиент двух сервисов и своей файловой области.
//! Читает запись собственного каталога сессией файловой системы, а дальше раз в
//! секунду спрашивает произведение у калькулятора, время у часов реального
//! времени и кладёт обе величины строкой в журнал.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

use core::panic::PanicInfo;

use calculator_demo::calculator;
use component_abi::{component_manifest, supervisor_gone};
use filesystem_abi::{AccessMode, OpenFlags};
use filesystem_client::FileSystem;
use ipc::{Transport as _, wire::IpcError};
use rtc_abi::rtc;
use runtime::{
    FileSystemEnvironment, OwnedHandle, PortTransport, SessionPort, Timeout, install_log, log_fmt,
    log_line, thread_exit,
};
use syscall::Handle;

const LOG_TAG: &str = "demo";

const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const TURN: Timeout = Timeout::from_secs(1);

const MULTIPLIER: u64 = 7;

const BANNER_RECORD: &str = "/banner";
const BANNER_MAX: usize = 64;

const NO_STARTUP_EXIT_CODE: u64 = 1;
const CALCULATOR_LOST_EXIT_CODE: u64 = 3;
const CLOCK_LOST_EXIT_CODE: u64 = 4;
const SUPERVISOR_LOST_EXIT_CODE: u64 = 7;

component_manifest! {
    struct DemoStartup {
        slots: {
            calculator: service(protocol = "Calculator"),
            clock: service(protocol = "Rtc"),
            area: directory(instance = "filesystem-imagefs"),
        },
    }
}

enum Supervisor {
    Alive,
    Gone,
}

/// `startup` приходит в x0 как HandleId одноразового startup-порта.
#[unsafe(no_mangle)]
pub extern "C" fn _start(startup: Handle) -> ! {
    let startup = OwnedHandle::from_handle(startup);
    let Ok(Some(started)) = DemoStartup::receive(&PortTransport::server(startup.borrow())) else {
        thread_exit(NO_STARTUP_EXIT_CODE);
    };
    drop(startup);

    install_log(OwnedHandle::adopt(started.log), LOG_TAG);
    // Серверная сторона сервисного порта живёт до конца процесса без
    // обслуживания: экспорт демо в план композиции не входит.
    let _service = OwnedHandle::adopt(started.service);
    let control = OwnedHandle::adopt(started.control);
    let calculator_session = OwnedHandle::adopt(started.calculator);
    let clock_session = OwnedHandle::adopt(started.clock);
    let area_session = OwnedHandle::adopt(started.area);

    let calculator =
        calculator::Client::new(PortTransport::client(&calculator_session)).with_wait(CALL_TIMEOUT);
    let clock = rtc::Client::new(PortTransport::client(&clock_session)).with_wait(CALL_TIMEOUT);

    log_line("started");
    announce(SessionPort::over(area_session));
    thread_exit(run(
        &calculator,
        &clock,
        &PortTransport::server(control.borrow()),
    ))
}

fn announce(area: SessionPort) {
    // Запись лежит в собственном каталоге компонента, и его область - корень
    // сессии: имя отсчитывается от неё.
    let mut bytes = [0; BANNER_MAX];
    match banner(area, &mut bytes) {
        Some(text) => log_fmt(format_args!("banner: {text}")),
        None => log_line("banner unread"),
    }
}

fn banner(area: SessionPort, bytes: &mut [u8; BANNER_MAX]) -> Option<&str> {
    let mut area = FileSystem::windowed(FileSystemEnvironment, area).with_wait(CALL_TIMEOUT);
    let mut record = area
        .open(BANNER_RECORD, AccessMode::Read, OpenFlags::NONE)
        .ok()?;
    let count = record.read(bytes).ok()?;

    Some(core::str::from_utf8(bytes.get(..count)?).ok()?.trim_end())
}

fn run(
    calculator: &calculator::Client<PortTransport>,
    clock: &rtc::Client<PortTransport>,
    control: &PortTransport,
) -> u64 {
    let mut step: u64 = 1;
    loop {
        // Уход провайдера виден отказом транспорта на его сессии.
        let Ok(product) = calculator.multiply(step, MULTIPLIER) else {
            log_line("calculator gone");
            return CALCULATOR_LOST_EXIT_CODE;
        };
        let Ok(now) = clock.now() else {
            log_line("clock gone");
            return CLOCK_LOST_EXIT_CODE;
        };
        log_fmt(format_args!(
            "{step} * {MULTIPLIER} = {product}, rtc {now}s"
        ));
        if matches!(turn(control), Supervisor::Gone) {
            log_line("supervisor gone");
            return SUPERVISOR_LOST_EXIT_CODE;
        }
        step = step.wrapping_add(1);
    }
}

fn turn(control: &PortTransport) -> Supervisor {
    // Срок приёма на control-порту задаёт такт цикла, а уход значка supervisor
    // виден отказом того же приёма.
    match control.wait_readable(TURN) {
        // Протокола на control-порту нет: пришедший кадр уходит без ответа, а
        // закрытие reply-объекта будит вызывателя.
        Ok(()) => {
            control.cancel_reply();
            Supervisor::Alive
        }
        // Покадровое исчерпание такт не рвёт, как и уход чужого значка.
        Err(IpcError::Timeout | IpcError::ResourceExhausted) => Supervisor::Alive,
        Err(IpcError::SenderGone) if !supervisor_gone(control.peer_badge()) => Supervisor::Alive,
        Err(_) => Supervisor::Gone,
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    thread_exit(1)
}
