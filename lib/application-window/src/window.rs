use alloc::rc::Rc;
use core::{
    cell::{Cell, RefCell},
    fmt,
    task::{Poll, Waker},
};

use application_abi::{ApplicationError, application};
use component_startup::{AcceptError, AsyncStartup, SessionPort, accept, serve_calls};
use display_abi::display;
use executor::{Executor, TaskHandle};
use input_abi::input;
use ipc::{CallError, Timeout};
use kernel_objects::{Delivery, ReceivedSession, Session};

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
    state: Rc<Application>,
    _serving: TaskHandle,
}

struct Application {
    shown: Cell<bool>,
    shown_window: RefCell<Option<ShownWindow>>,
    closed: Cell<bool>,
    ended: Cell<bool>,
    changed: RefCell<Option<Waker>>,
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

impl Application {
    fn wake(&self) {
        let waker = self.changed.borrow_mut().take();
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn end(&self) {
        self.ended.set(true);
        self.wake();
    }
}

#[allow(clippy::unused_async_trait_impl)]
impl application::Service<Delivery> for Application {
    fn show(
        &self,
        _frame: &Delivery,
        window: ReceivedSession<display::Protocol>,
        input: ReceivedSession<input::Protocol>,
    ) -> Result<(), ApplicationError> {
        if self.shown.replace(true) {
            return Err(ApplicationError::Refused);
        }

        *self.shown_window.borrow_mut() = Some(ShownWindow {
            window: Session::from_session(window),
            input: Session::from_session(input),
        });
        self.wake();
        Ok(())
    }

    async fn closed(&self, _frame: &Delivery) -> Result<(), ApplicationError> {
        if self.closed.replace(true) {
            return Err(ApplicationError::Refused);
        }
        core::future::pending().await
    }
}

impl ApplicationSession {
    /// Ждёт закрытия сессии открывшим приложение. Незавершённые вызовы отменяются
    /// задачей обслуживания перед возвратом.
    pub async fn serve(self) {
        core::future::poll_fn(|cx| {
            if self.state.ended.get() {
                return Poll::Ready(());
            }
            *self.state.changed.borrow_mut() = Some(cx.waker().clone());
            Poll::Pending
        })
        .await;
    }
}

/// Принимает заявку на экспорт `Application` от родителя `parent` и возвращает окно и ввод
/// из вызова `show`. `executor` обслуживает сессию до разрушения `ApplicationSession`.
pub async fn accept_window(
    executor: &Rc<Executor>,
    parent: AsyncStartup,
) -> Result<ApplicationWindow, WindowError> {
    let service = Rc::new(SessionPort::create().map_err(WindowError::Kernel)?);
    let request = accept::<application::Protocol>(&parent.with_wait(Timeout::INFINITE))
        .await
        .map_err(WindowError::Accept)?;
    let session = service.create_session().map_err(WindowError::Kernel)?;
    let _ = request.reply.send(Ok(session.port));

    let state = Rc::new(Application {
        shown: Cell::new(false),
        shown_window: RefCell::new(None),
        closed: Cell::new(false),
        ended: Cell::new(false),
        changed: RefCell::new(None),
    });
    let calls = state.clone();
    let gone = state.clone();
    let serving = serve_calls(
        executor,
        service,
        move |frame| application::serve(&calls, frame),
        move |_| gone.end(),
    );
    let stopped = state.clone();
    let serving = executor
        .spawn_now(async move {
            let _ = serving.await;
            stopped.end();
        })
        .abort_on_drop();

    let ShownWindow { window, input } = core::future::poll_fn(|cx| {
        if let Some(shown) = state.shown_window.borrow_mut().take() {
            return Poll::Ready(Ok(shown));
        }

        if state.ended.get() {
            return Poll::Ready(Err(WindowError::Closed));
        }
        *state.changed.borrow_mut() = Some(cx.waker().clone());
        Poll::Pending
    })
    .await?;

    Ok(ApplicationWindow {
        window,
        input,
        session: ApplicationSession {
            state,
            _serving: serving,
        },
    })
}
