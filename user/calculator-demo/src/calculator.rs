//! Калькулятор: компонент композиции, экспортирующий протокол `Calculator`.
//! Лог-сессия, control-порт и сервисный порт приходят базовым кадром стартовой
//! транзакции, и слоты компоненту не нужны.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

use core::panic::PanicInfo;

use calculator_demo::{CalculatorApiService, dispatch_calculator_api};
use component_abi::component_manifest;
use ipc::{Transport as _, wire::IpcError};
use runtime::{
    OwnedHandle, PortTransport, Priority, Thread, Timeout, exit_with_sibling, install_log, log_fmt,
    log_line, spawn_owned, thread_exit,
};
use syscall::Handle;

const LOG_TAG: &str = "calculator";

const CONTROL_STACK: u64 = 0x4000;

const NO_STARTUP_EXIT_CODE: u64 = 1;
const NO_CONTROL_WATCH_EXIT_CODE: u64 = 2;
const CONTROL_PORT_FAILED_EXIT_CODE: u32 = 3;
const SERVICE_PORT_FAILED_EXIT_CODE: u32 = 4;
const SUPERVISOR_LOST_EXIT_CODE: u32 = 7;

component_manifest! {
    struct CalculatorStartup {
        exports: "Calculator",
        slots: {},
    }
}

struct Calculator;

impl CalculatorApiService for Calculator {
    fn multiply(&mut self, a: u64, b: u64) -> u64 {
        a * b
    }
}

/// `startup` приходит в x0 как HandleId одноразового startup-порта.
#[unsafe(no_mangle)]
pub extern "C" fn _start(startup: Handle) -> ! {
    let startup = OwnedHandle::from_handle(startup);
    let Ok(Some(started)) = CalculatorStartup::receive(&PortTransport::server(startup.borrow()))
    else {
        thread_exit(NO_STARTUP_EXIT_CODE);
    };
    drop(startup);

    install_log(OwnedHandle::adopt(started.log), LOG_TAG);
    let control = OwnedHandle::adopt(started.control);
    let service = OwnedHandle::adopt(started.service);

    let Ok(service_thread) = Thread::self_thread() else {
        thread_exit(NO_CONTROL_WATCH_EXIT_CODE);
    };
    // Поток control-порта живёт до конца процесса: join ему не нужен, а стек
    // он держит своим.
    let Ok(watch) = spawn_owned(
        move || watch_supervisor(&control, &service_thread),
        CONTROL_STACK,
        Priority::new(1),
    ) else {
        thread_exit(NO_CONTROL_WATCH_EXIT_CODE);
    };

    log_line("started");
    serve(&service);
    // Обслуживать больше некому, и уход одного потока процесс не снимает:
    // supervisor узнаёт об уходе компонента только по его коду выхода.
    exit_with_sibling(watch.thread(), SERVICE_PORT_FAILED_EXIT_CODE)
}

fn serve(service: &OwnedHandle) {
    let transport = PortTransport::server(service.borrow());
    let mut calculator = Calculator;
    // Порт остаётся открытым, пока кадр доставлен, значок ушёл либо бюджет
    // кадра исчерпан. Прочий отказ закрывает обслуживание.
    while matches!(
        dispatch_calculator_api(&mut calculator, &transport),
        Ok(()) | Err(IpcError::SenderGone | IpcError::ResourceExhausted | IpcError::Timeout)
    ) {}
    log_line("service port closed");
}

fn watch_supervisor(control: &OwnedHandle, service_thread: &Thread) -> u32 {
    let transport = PortTransport::server(control.borrow());
    loop {
        match transport.wait_readable(Timeout::INFINITE) {
            // Протокола на control-порту нет: пришедший кадр уходит без ответа,
            // а закрытие reply-объекта будит вызывателя.
            Ok(()) => transport.cancel_reply(),
            // Отправитель на control-порту один - поднявший компонент
            // supervisor. Его уход снимает процесс целиком.
            Err(IpcError::SenderGone) => {
                log_line("supervisor gone");
                exit_with_sibling(service_thread, SUPERVISOR_LOST_EXIT_CODE)
            }
            // Покадровое исчерпание и пробуждение без кадра цикл не закрывают.
            Err(IpcError::ResourceExhausted | IpcError::Timeout) => {}
            // Прочий отказ приёма оставляет калькулятор без надзора и снимает
            // процесс отличимым кодом.
            Err(_) => {
                log_line("control port failed");
                exit_with_sibling(service_thread, CONTROL_PORT_FAILED_EXIT_CODE)
            }
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    thread_exit(1)
}
