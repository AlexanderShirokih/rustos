//! Калькулятор: компонент композиции, экспортирующий протокол `Calculator`.
//! Калькулятор сам создаёт порт экспорта, а сессии экспорта по заявкам родителя
//! выдаёт отдельный поток.

use core::error::Error;
use std::{process, thread};

use calculator_demo::calculator;
use component_startup::{serve_export, whole_export};
use components::component_manifest;
use ipc::{Listener, RecvError, Timeout, wire::MESSAGE_INLINE_MAX};
use kernel_objects::{Port, PortTransport};

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

fn main() -> Result<(), Box<dyn Error>> {
    let CalculatorStartup { parent } = components::requirements::<CalculatorStartup>()?;
    let service = Port::create()?;

    let port = &service;
    thread::scope(|scope| {
        scope.spawn(move || {
            let failure = serve_export::<calculator::Protocol>(&parent, whole_export(port));
            println!("export stopped: {failure:?}");
            process::exit(1);
        });
        println!("started");
        serve(port);
        process::exit(1);
    });
    Ok(())
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
