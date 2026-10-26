use std::{process, rc::Rc};

use application_window::{ApplicationWindow, WindowError};
use component_startup::{AsyncStartup, component_startup as startup_contract};
use display_abi::display;
use executor::{Executor, PortChannel};
use input_abi::input;
use kernel_objects::{PortTransport, Session};

/// Окно и ввод приложения.
pub struct Shown {
    pub window: Session<display::Protocol>,
    pub input: Session<input::Protocol>,
}

/// Получает окно и ввод приложения.
pub async fn receive_window(
    executor: &Rc<Executor>,
    parent: startup_contract::Client<PortTransport>,
) -> Result<Shown, WindowError> {
    let parent = AsyncStartup::new(PortChannel::from_transport(
        parent.into_channel(),
        executor.reactor(),
    ));
    let ApplicationWindow {
        window,
        input,
        session,
    } = application_window::receive_window(executor, parent).await?;

    executor.spawn(async move {
        session.serve().await;
        process::exit(0)
    });
    Ok(Shown { window, input })
}
