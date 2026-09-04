//! Калькулятор: компонент композиции, экспортирующий протокол `Calculator`.
//! Лог-сессия, control-порт и сервисный порт приходят стартовым набором, и слоты
//! компоненту не нужны.

use std::{process, thread};

use calculator_demo::calculator;
use component_abi::supervisor_gone;
use component_client::component_manifest;
use ipc::{Transport as _, wire::IpcError};
use kernel_objects::{OwnedHandle, PortTransport};
use syscall::Timeout;

const NO_STARTUP_EXIT_CODE: i32 = 1;
const CONTROL_PORT_FAILED_EXIT_CODE: i32 = 3;
const SERVICE_PORT_FAILED_EXIT_CODE: i32 = 4;
const SUPERVISOR_LOST_EXIT_CODE: i32 = 7;

component_manifest! {
    struct CalculatorStartup {
        exports: "Calculator",
        slots: {},
    }
}

struct Calculator;

impl calculator::Service for Calculator {
    fn multiply(&mut self, a: u64, b: u64) -> u64 {
        a * b
    }
}

fn main() {
    // До установки лог-сессии вывод не доходит до журнала, поэтому процесс называет отказ
    // приёма кодом выхода.
    let Ok(CalculatorStartup { control, service }) =
        component_client_std::receive(CalculatorStartup::receive)
    else {
        process::exit(NO_STARTUP_EXIT_CODE);
    };

    // Поток control-порта живёт до конца процесса: join ему не нужен.
    thread::spawn(move || watch_supervisor(&control));

    println!("started");
    serve(&service);
    // Обслуживать больше некому, и уход одного потока процесс не снимает:
    // supervisor узнаёт об уходе компонента только по его коду выхода.
    process::exit(SERVICE_PORT_FAILED_EXIT_CODE)
}

fn serve(service: &OwnedHandle) {
    let transport = PortTransport::server(service);
    let mut calculator = Calculator;
    // Порт остаётся открытым, пока кадр доставлен, значок ушёл либо бюджет
    // кадра исчерпан. Прочий отказ закрывает обслуживание.
    while matches!(
        calculator::dispatch(&mut calculator, &transport),
        Ok(()) | Err(IpcError::SenderGone | IpcError::ResourceExhausted | IpcError::Timeout)
    ) {}
    println!("service port closed");
}

fn watch_supervisor(control: &OwnedHandle) -> ! {
    let transport = PortTransport::server(control);
    loop {
        match transport.wait_readable(Timeout::INFINITE) {
            // Протокола на control-порту нет: пришедший кадр уходит без ответа,
            // а закрытие reply-объекта будит вызывателя.
            Ok(()) => transport.cancel_reply(),
            // Уход значка поднявшего компонент supervisor снимает процесс целиком.
            Err(IpcError::SenderGone) if supervisor_gone(transport.peer_badge()) => {
                println!("supervisor gone");
                process::exit(SUPERVISOR_LOST_EXIT_CODE)
            }
            // Покадровое исчерпание, пробуждение без кадра и уход чужого значка цикл не
            // закрывают.
            Err(IpcError::SenderGone | IpcError::ResourceExhausted | IpcError::Timeout) => {}
            // Прочий отказ приёма оставляет калькулятор без надзора и снимает
            // процесс отличимым кодом.
            Err(_) => {
                println!("control port failed");
                process::exit(CONTROL_PORT_FAILED_EXIT_CODE)
            }
        }
    }
}
