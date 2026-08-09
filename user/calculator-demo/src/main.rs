//! Демо композиции: компонент-клиент двух сервисов. Раз в секунду спрашивает
//! произведение у калькулятора, время у часов реального времени и кладёт обе
//! величины строкой в журнал.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

use core::panic::PanicInfo;

use calculator_demo::CalculatorApiClient;
use component_abi::component_manifest;
use ipc::{Transport as _, wire::IpcError};
use rtc_abi::RtcClient;
use runtime::{OwnedHandle, PortTransport, Timeout, install_log, log_fmt, log_line, thread_exit};
use syscall::Handle;

const LOG_TAG: &str = "demo";

const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const TURN: Timeout = Timeout::from_secs(1);

const MULTIPLIER: u64 = 7;

const NO_STARTUP_EXIT_CODE: u64 = 1;
const CALCULATOR_LOST_EXIT_CODE: u64 = 3;
const CLOCK_LOST_EXIT_CODE: u64 = 4;
const SUPERVISOR_LOST_EXIT_CODE: u64 = 7;

component_manifest! {
    struct DemoStartup {
        slots: {
            calculator: service(protocol = "Calculator"),
            clock: service(protocol = "Rtc"),
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

    let calculator = CalculatorApiClient::new(PortTransport::client(&calculator_session))
        .with_wait(CALL_TIMEOUT);
    let clock = RtcClient::new(PortTransport::client(&clock_session)).with_wait(CALL_TIMEOUT);

    log_line("started");
    thread_exit(run(
        &calculator,
        &clock,
        &PortTransport::server(control.borrow()),
    ))
}

fn run(
    calculator: &CalculatorApiClient<PortTransport>,
    clock: &RtcClient<PortTransport>,
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
    // Срок приёма на control-порту задаёт такт цикла, а уход supervisor -
    // единственного отправителя порта - виден отказом того же приёма.
    match control.wait_readable(TURN) {
        // Протокола на control-порту нет: пришедший кадр уходит без ответа, а
        // закрытие reply-объекта будит вызывателя.
        Ok(()) => {
            control.cancel_reply();
            Supervisor::Alive
        }
        // Покадровое исчерпание такт не рвёт.
        Err(IpcError::Timeout | IpcError::ResourceExhausted) => Supervisor::Alive,
        Err(_) => Supervisor::Gone,
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    thread_exit(1)
}
