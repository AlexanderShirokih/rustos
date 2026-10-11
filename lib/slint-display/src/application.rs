use std::{process, thread};

use application_window::{ApplicationWindow, WindowError};
use component_startup::component_startup as startup_contract;
use display_abi::display;
use input_abi::input;
use ipc::Timeout;
use kernel_objects::{PortTransport, SessionTransport};

/// Окно и ввод приложения из вызова `show`.
pub struct Shown {
    pub window: display::Client<SessionTransport>,
    pub input: input::Client<SessionTransport>,
}

/// Принимает заявку на экспорт `Application` от родителя `parent` и возвращает окно и ввод из
/// её вызова `show`. Дальше сессию `Application` обслуживает отдельный поток. Он отвечает
/// `Refused` на повторный `show` и завершает процесс кодом 0, когда открывший приложение
/// закрывает сессию.
pub fn accept_window(
    parent: startup_contract::Client<PortTransport>,
) -> Result<Shown, WindowError> {
    let ApplicationWindow {
        window,
        input,
        session,
    } = application_window::accept_window(parent)?;
    thread::spawn(move || {
        session.serve();
        process::exit(0)
    });
    Ok(Shown {
        window: window.client(),
        input: input.client().with_wait(Timeout::INFINITE),
    })
}
