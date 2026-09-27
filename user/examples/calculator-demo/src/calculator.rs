//! Калькулятор: компонент композиции, экспортирующий протокол `Calculator`.
//! Калькулятор сам создаёт порт экспорта, а сессии экспорта по заявкам родителя
//! выдаёт отдельный поток.

use std::{process, thread};

use calculator_demo::calculator;
use component_startup::{component_manifest, serve_export, whole_export};
use ipc::{Listener, RecvError, Timeout, wire::MESSAGE_INLINE_MAX};
use kernel_objects::{Port, PortTransport};

const NO_STARTUP_EXIT_CODE: i32 = 1;
const SERVICE_PORT_FAILED_EXIT_CODE: i32 = 4;
const EXPORT_FAILED_EXIT_CODE: i32 = 6;

component_manifest! {
    struct CalculatorStartup {
        exports: [calculator::Protocol],
    }
}

struct Calculator;

impl<F: ipc::ReceivedResources> calculator::Service<F> for Calculator {
    fn multiply(&mut self, _frame: &F, a: u64, b: u64) -> u64 {
        a * b
    }
}

fn main() {
    // Без журнала вывод `println!` теряется, и отказ старта передаётся кодом выхода.
    let Ok(CalculatorStartup { parent }) = component_startup_std::start(CalculatorStartup::start)
    else {
        process::exit(NO_STARTUP_EXIT_CODE);
    };
    let Ok(service) = Port::create() else {
        process::exit(SERVICE_PORT_FAILED_EXIT_CODE);
    };

    let port = &service;
    thread::scope(|scope| {
        scope.spawn(move || {
            let failure = serve_export::<calculator::Protocol>(&parent, whole_export(port));
            println!("export stopped: {failure:?}");
            process::exit(EXPORT_FAILED_EXIT_CODE);
        });
        println!("started");
        serve(port);
        process::exit(SERVICE_PORT_FAILED_EXIT_CODE);
    });
}

fn serve(service: &Port) {
    let transport = PortTransport::new(service);
    let mut calculator = Calculator;
    let mut bytes = [0u8; MESSAGE_INLINE_MAX];
    // Порт остаётся открытым, пока кадр доставлен, значок ушёл либо кадр отброшен.
    // Закрытие порта заканчивает обслуживание.
    loop {
        match transport.recv(&mut bytes, Timeout::INFINITE) {
            Ok(frame) => {
                let _ = calculator::dispatch(&mut calculator, frame);
            }
            Err(RecvError::SenderGone(_) | RecvError::Discarded | RecvError::Timeout) => {}
            Err(RecvError::Closed) => break,
        }
    }
    println!("service port closed");
}
