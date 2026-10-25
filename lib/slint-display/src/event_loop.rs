//! Платформа, которую получает Slint: окно программы, время с запуска и очередь задач
//! главного потока.

use core::{
    task::{Poll, Waker},
    time::Duration,
};
use std::{
    collections::VecDeque,
    rc::Rc,
    sync::{Arc, Mutex},
    time::Instant,
};

use slint::{
    EventLoopError, PlatformError,
    platform::{EventLoopProxy, Platform, WindowAdapter, software_renderer::MinimalSoftwareWindow},
};

type Task = Box<dyn FnOnce() + Send>;

#[derive(Clone, Default)]
pub(crate) struct TaskQueue(Arc<Mutex<Pending>>);

#[derive(Default)]
struct Pending {
    tasks: VecDeque<Task>,
    revision: u64,
    waker: Option<Waker>,
}

pub(crate) struct EventLoop {
    window: Rc<MinimalSoftwareWindow>,
    start: Instant,
    tasks: TaskQueue,
}

impl TaskQueue {
    pub(crate) fn run_pending(&self) -> u64 {
        let (pending, revision) = self
            .0
            .lock()
            .map(|mut state| (core::mem::take(&mut state.tasks), state.revision))
            .unwrap_or_default();

        for task in pending {
            task();
        }
        revision
    }

    pub(crate) fn notify(&self) {
        let waker = self.0.lock().ok().and_then(|mut state| {
            state.revision = state.revision.wrapping_add(1);
            state.waker.take()
        });
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    pub(crate) async fn changed(&self, revision: u64) {
        core::future::poll_fn(|cx| {
            let Ok(mut state) = self.0.lock() else {
                return Poll::Ready(());
            };

            if state.revision != revision || !state.tasks.is_empty() {
                return Poll::Ready(());
            }

            state.waker = Some(cx.waker().clone());
            Poll::Pending
        })
        .await;
    }
}

impl EventLoopProxy for TaskQueue {
    fn quit_event_loop(&self) -> Result<(), EventLoopError> {
        Err(EventLoopError::NoEventLoopProvider)
    }

    fn invoke_from_event_loop(&self, event: Task) -> Result<(), EventLoopError> {
        let waker = {
            let mut state = self
                .0
                .lock()
                .map_err(|_| EventLoopError::EventLoopTerminated)?;
            state.tasks.push_back(event);
            state.revision = state.revision.wrapping_add(1);
            state.waker.take()
        };

        if let Some(waker) = waker {
            waker.wake();
        }

        Ok(())
    }
}

impl EventLoop {
    pub(crate) fn new(window: Rc<MinimalSoftwareWindow>, tasks: TaskQueue) -> Self {
        Self {
            window,
            start: Instant::now(),
            tasks,
        }
    }
}

impl Platform for EventLoop {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> Duration {
        self.start.elapsed()
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(self.tasks.clone()))
    }
}
