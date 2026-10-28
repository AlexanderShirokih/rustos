use alloc::{boxed::Box, rc::Rc, vec::Vec};
use core::{borrow::Borrow, fmt, future::Future, pin::Pin, task::Poll};

use executor::{Executor, TaskHandle, yield_now};
use ipc::{Incoming, wire::IpcError};
use kernel_objects::{Delivery, Port};

use crate::{
    AsyncStartup, Attributes, ExportStopped, SessionError, SessionEvent, SessionPort, serve_export,
};

const FRAMES_PER_TURN: usize = 16;

/// Причина остановки приёма сессий или заявок экспорта.
#[derive(Debug)]
pub enum ServeStopped {
    /// Родитель прекратил выдавать заявки экспорта.
    Export(ExportStopped),
    /// Порт сессий закрылся.
    Closed,
    /// Ядро отказало в ожидании порта сессий.
    Kernel(kernel_objects::Error),
}

impl fmt::Display for ServeStopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Export(error) => error.fmt(formatter),
            Self::Closed => formatter.write_str("session port closed"),
            Self::Kernel(error) => error.fmt(formatter),
        }
    }
}

async fn serve_sessions(
    port: &SessionPort,
    mut handle: impl FnMut(SessionEvent<'_>),
) -> ServeStopped {
    let mut events = port.events();
    let mut served = 0usize;

    loop {
        match events.recv().await {
            Ok(Some(event)) => {
                handle(event);
                served += 1;
                if served == FRAMES_PER_TURN {
                    served = 0;
                    yield_now().await;
                }
            }
            Ok(None) => return ServeStopped::Closed,
            Err(error) => return ServeStopped::Kernel(error),
        }
    }
}

/// Обслуживает вызовы сессий порта `sessions` на одном executor.
///
/// `handle` получает кадр в буфере приёма, как `serve` контракта. `Ok(None)` - вызов обслужен
/// на месте. Future из `Ok(Some)` владеет кадром и выполняется задачей executor. Вызовы одной
/// сессии могут ждать одновременно. Уход значка отменяет его задачи до `gone`. Остановка порта
/// отменяет оставшиеся задачи. Future обслуживания не удерживает executor.
pub fn serve_calls<F: Future<Output = Result<(), IpcError>> + 'static>(
    executor: &Rc<Executor>,
    sessions: impl Borrow<SessionPort> + 'static,
    handle: impl Fn(Incoming<'_, Delivery>) -> Result<Option<F>, IpcError> + 'static,
    mut gone: impl FnMut(u64) + 'static,
) -> impl Future<Output = ServeStopped> + 'static {
    let executor = Rc::downgrade(executor);

    async move {
        if executor.upgrade().is_none() {
            return ServeStopped::Closed;
        }

        let mut calls: Vec<(u64, TaskHandle<Result<(), IpcError>>)> = Vec::new();

        serve_sessions(sessions.borrow(), |event| match event {
            SessionEvent::Frame(frame) => {
                let badge = frame.resources.badge;
                if let Some(future) = start(&handle, frame)
                    && let Some(executor) = executor.upgrade()
                {
                    calls.retain(|(_, task)| !task.is_finished());
                    calls.push((badge, executor.spawn_pinned(future).abort_on_drop()));
                }
            }
            SessionEvent::Gone(badge) => {
                calls.retain(|(owner, task)| *owner != badge && !task.is_finished());
                gone(badge);
            }
        })
        .await
    }
}

#[inline(never)]
fn start<F: Future>(
    handle: &impl Fn(Incoming<'_, Delivery>) -> Result<Option<F>, IpcError>,
    frame: Incoming<'_, Delivery>,
) -> Option<Pin<Box<F>>> {
    handle(frame).ok().flatten().map(Box::pin)
}

/// Обслуживает экспорт протокола `P` по стартовой сессии `parent` и вызовы его сессий, как
/// [`serve_calls`]. Остановка экспорта отменяет оставшиеся вызовы.
pub fn serve_component<P: ipc::Protocol, F: Future<Output = Result<(), IpcError>> + 'static>(
    executor: &Rc<Executor>,
    parent: AsyncStartup,
    sessions: impl Borrow<SessionPort> + 'static,
    handle: impl Fn(Incoming<'_, Delivery>) -> Result<Option<F>, IpcError> + 'static,
    open: impl AsyncFnMut(Attributes<'_>) -> Result<Port, SessionError> + 'static,
    gone: impl FnMut(u64) + 'static,
) -> impl Future<Output = ServeStopped> + 'static {
    let calls = serve_calls(executor, sessions, handle, gone);

    async move {
        let mut export = Box::pin(serve_export::<P>(parent, open));
        let mut calls = core::pin::pin!(calls);

        core::future::poll_fn(|cx| {
            if let Poll::Ready(error) = export.as_mut().poll(cx) {
                return Poll::Ready(ServeStopped::Export(error));
            }

            calls.as_mut().poll(cx)
        })
        .await
    }
}
