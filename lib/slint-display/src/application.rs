use std::{process, rc::Rc};

use application_window::{ApplicationWindow, WindowError};
use component_startup::{AsyncStartup, component_startup as startup_contract};
use display_abi::display;
use executor::{Executor, PortChannel};
use input_abi::input;
use kernel_objects::{PortTransport, Session};

/// Окно и ввод приложения из вызова `show`.
pub struct Shown {
    pub window: Session<display::Protocol>,
    pub input: Session<input::Protocol>,
}

/// Получает окно и ввод из `show`. Задача сессии `Application` отвечает `Refused` на
/// повторный `show` и завершает процесс кодом 0 при закрытии сессии.
pub async fn accept_window(
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
    } = application_window::accept_window(executor, parent).await?;

    executor.spawn(async move {
        session.serve().await;
        process::exit(0)
    });
    Ok(Shown { window, input })
}
