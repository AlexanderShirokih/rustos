use core::error::Error;
use std::rc::Rc;

use calculator_demo::calculator;
use component_startup::{AsyncStartup, SessionPort, serve_component, whole_export};
use components::component_manifest;
use executor::{Executor, PortChannel};

component_manifest! {
    struct CalculatorStartup {
        exports: [calculator::Protocol],
    }
}

struct Calculator;

#[allow(clippy::unused_async_trait_impl)]
impl<F: ipc::ReceivedResources> calculator::Service<F> for Calculator {
    fn multiply(&self, _frame: &F, a: u64, b: u64) -> u64 {
        a * b
    }
}

pub(crate) fn main() -> Result<(), Box<dyn Error>> {
    let CalculatorStartup { parent } = components::requirements::<CalculatorStartup>()?;
    let executor = Rc::new(Executor::new()?);
    let parent = AsyncStartup::new(PortChannel::from_transport(parent.into_channel()));
    let service = Rc::new(SessionPort::create()?);
    let opened = service.clone();
    let calls = Rc::new(Calculator);

    let served = serve_component::<calculator::Protocol, _>(
        &executor,
        parent,
        service,
        move |frame| calculator::serve(&calls, frame),
        whole_export(opened),
        |_| {},
    );

    println!("started");
    let stopped = executor.block_on(served);
    Err(stopped.to_string().into())
}
