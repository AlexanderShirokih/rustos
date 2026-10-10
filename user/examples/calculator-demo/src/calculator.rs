//! Калькулятор: компонент композиции, экспортирующий протокол `Calculator`.
//! Калькулятор сам создаёт порт экспорта, а сессии экспорта по заявкам родителя
//! выдаёт отдельный поток.

use core::error::Error;
use std::{process, thread};

use calculator_demo::calculator;
use component_startup::{SessionEvent, SessionEvents, serve_export, whole_export};
use components::component_manifest;
use kernel_objects::Port;

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
    let mut calculator = Calculator;
    let mut events = SessionEvents::new(service);

    while let Some(event) = events.recv() {
        if let SessionEvent::Frame(frame) = event {
            let _ = calculator::dispatch(&mut calculator, frame);
        }
    }
    println!("service port closed");
}
