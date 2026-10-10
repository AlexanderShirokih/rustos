//! Платформа, которую получает Slint: окно программы, время с запуска и очередь задач
//! главного потока.

use core::time::Duration;
use std::{
    collections::VecDeque,
    rc::Rc,
    sync::{Arc, Condvar, Mutex},
    time::Instant,
};

use slint::{
    EventLoopError, PlatformError,
    platform::{EventLoopProxy, Platform, WindowAdapter, software_renderer::MinimalSoftwareWindow},
};

type Task = Box<dyn FnOnce() + Send>;

#[derive(Clone, Default)]
pub(crate) struct TaskQueue(Arc<Pending>);

#[derive(Default)]
struct Pending {
    tasks: Mutex<VecDeque<Task>>,
    posted: Condvar,
}

pub(crate) struct EventLoop {
    window: Rc<MinimalSoftwareWindow>,
    start: Instant,
    tasks: TaskQueue,
}

impl TaskQueue {
    pub(crate) fn run_pending(&self) {
        let pending = self
            .0
            .tasks
            .lock()
            .map(|mut queue| core::mem::take(&mut *queue))
            .unwrap_or_default();
        for task in pending {
            task();
        }
    }

    pub(crate) fn wait_for_task(&self, timeout: Option<Duration>) {
        let Ok(tasks) = self.0.tasks.lock() else {
            return;
        };
        let empty = |tasks: &mut VecDeque<Task>| tasks.is_empty();

        match timeout {
            Some(timeout) => drop(self.0.posted.wait_timeout_while(tasks, timeout, empty)),
            None => drop(self.0.posted.wait_while(tasks, empty)),
        }
    }
}

impl EventLoopProxy for TaskQueue {
    fn quit_event_loop(&self) -> Result<(), EventLoopError> {
        Err(EventLoopError::NoEventLoopProvider)
    }

    fn invoke_from_event_loop(&self, event: Task) -> Result<(), EventLoopError> {
        self.0
            .tasks
            .lock()
            .map_err(|_| EventLoopError::EventLoopTerminated)?
            .push_back(event);
        self.0.posted.notify_one();

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
