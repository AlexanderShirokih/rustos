use core::fmt;

use application_abi::{ApplicationError, application};
use component_startup::{
    AcceptError, SessionEvent, SessionPort, component_startup as startup_contract,
};
use display_abi::display;
use input_abi::input;
use ipc::{CallError, Timeout};
use kernel_objects::{Delivery, IssuedHandle, PortTransport, ReceivedSession, Session, kind};

/// Отказ получения окна приложения.
#[derive(Debug)]
pub enum WindowError {
    /// Родитель отказал в заявке на экспорт `Application`.
    Accept(CallError<AcceptError>),
    /// Ядро отказало в порте сессии `Application`.
    Kernel(kernel_objects::Error),
    /// Сессия `Application` закрылась до `show`.
    Closed,
}

/// Окно и ввод из вызова `show` и сессия `Application`, в которой они пришли.
pub struct ApplicationWindow {
    pub window: Session<display::Protocol>,
    pub input: Session<input::Protocol>,
    pub session: ApplicationSession,
}

/// Сессия `Application` после `show`.
pub struct ApplicationSession {
    service: SessionPort,
    application: Application,
}

struct Application {
    shown: bool,
    shown_window: Option<ShownWindow>,
    closed: Option<application::closed::Reply<IssuedHandle<kind::Reply>>>,
}

struct ShownWindow {
    window: Session<display::Protocol>,
    input: Session<input::Protocol>,
}

impl fmt::Display for WindowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Accept(error) => write!(formatter, "application export refused: {error:?}"),
            Self::Kernel(error) => error.fmt(formatter),
            Self::Closed => formatter.write_str("application session closed before show"),
        }
    }
}

impl core::error::Error for WindowError {}

impl application::Service<Delivery> for Application {
    fn show(
        &mut self,
        _frame: &Delivery,
        window: ReceivedSession<display::Protocol>,
        input: ReceivedSession<input::Protocol>,
    ) -> Result<(), ApplicationError> {
        if self.shown {
            return Err(ApplicationError::Refused);
        }

        self.shown = true;
        self.shown_window = Some(ShownWindow {
            window: Session::from_session(window),
            input: Session::from_session(input),
        });
        Ok(())
    }

    fn closed(
        &mut self,
        _frame: &Delivery,
        reply: application::closed::Reply<IssuedHandle<kind::Reply>>,
    ) {
        if self.closed.is_some() {
            let _ = reply.send(Err::<(), _>(ApplicationError::Refused));
            return;
        }
        self.closed = Some(reply);
    }
}

impl ApplicationSession {
    /// Отвечает `Refused` на повторный `show` и возвращается, когда открывший приложение
    /// закрывает сессию.
    pub fn serve(mut self) {
        let mut events = self.service.events();

        while let Some(event) = events.recv() {
            match event {
                SessionEvent::Frame(frame) => {
                    let _ = application::dispatch(&mut self.application, frame);
                }
                SessionEvent::Gone(_) => return,
            }
        }
    }
}

/// Принимает заявку на экспорт `Application` от родителя `parent` и возвращает окно и ввод из
/// её вызова `show` вместе с сессией `Application`.
pub fn accept_window(
    parent: startup_contract::Client<PortTransport>,
) -> Result<ApplicationWindow, WindowError> {
    let parent = parent.with_wait(Timeout::INFINITE);
    let service = SessionPort::create().map_err(WindowError::Kernel)?;
    let request =
        component_startup::accept::<application::Protocol>(&parent).map_err(WindowError::Accept)?;
    let session = service.create_session().map_err(WindowError::Kernel)?;
    let _ = request.reply.send(Ok(session.port));

    let mut application = Application {
        shown: false,
        shown_window: None,
        closed: None,
    };
    let ShownWindow { window, input } = wait_show(&service, &mut application)?;

    Ok(ApplicationWindow {
        window,
        input,
        session: ApplicationSession {
            service,
            application,
        },
    })
}

fn wait_show(
    service: &SessionPort,
    application: &mut Application,
) -> Result<ShownWindow, WindowError> {
    let mut events = service.events();

    loop {
        let Some(SessionEvent::Frame(frame)) = events.recv() else {
            return Err(WindowError::Closed);
        };
        let _ = application::dispatch(application, frame);
        if let Some(shown) = application.shown_window.take() {
            return Ok(shown);
        }
    }
}
