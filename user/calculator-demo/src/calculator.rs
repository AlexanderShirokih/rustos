//! Калькулятор: компонент композиции, экспортирующий протокол `Calculator`.
//! Лог-сессия, startup-порт и порт экспорта приходят стартовым набором, и слоты
//! компоненту не нужны.

use std::process;

use calculator_demo::calculator;
use component_startup::component_manifest;
use ipc::wire::IpcError;
use kernel_objects::{Port, PortTransport};

const NO_STARTUP_EXIT_CODE: i32 = 1;
const SERVICE_PORT_FAILED_EXIT_CODE: i32 = 4;

component_manifest! {
    struct CalculatorStartup {
        exports: { service: "Calculator" },
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
    let Ok(CalculatorStartup { startup, service }) =
        component_startup_std::receive(CalculatorStartup::receive)
    else {
        process::exit(NO_STARTUP_EXIT_CODE);
    };

    // Startup-порт открыт всю incarnation, и его серверная сторона держится до конца процесса.
    let _startup = startup;

    println!("started");
    serve(&service);
    // Обслуживать больше некому, и уход одного потока процесс не снимает:
    // supervisor узнаёт об уходе компонента только по его коду выхода.
    process::exit(SERVICE_PORT_FAILED_EXIT_CODE)
}

fn serve(service: &Port) {
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
