//! Сессия Input, модель контроллера и очередь движка под одним mutex.

use alloc::collections::VecDeque;

use doomgeneric::{Controls, InputState, KeyEvent, Layout, Mode};
use input_abi::{Kind, input};
use kernel_objects::Session;
use runtime::{Mutex, Timeout, log_fmt, spawn_owned};
use syscall::SyscallError;

static EVENTS: Mutex<Events> = Mutex::new(Events {
    queue: VecDeque::new(),
    state: None,
});

struct Events {
    queue: VecDeque<KeyEvent>,
    state: Option<InputState>,
}

pub(crate) fn start(session: Session<input::Protocol>, layout: Layout) -> Result<(), SyscallError> {
    EVENTS.lock().state = Some(InputState::new(layout));
    let client = session.client().with_wait(Timeout::INFINITE);
    spawn_owned(move || {
        loop {
            let records = match client.next() {
                Ok(records) => records,
                Err(error) => {
                    log_fmt(format_args!("input stopped: {error:?}"));
                    let mut events = EVENTS.lock();
                    let Events { state, queue } = &mut *events;
                    if let Some(state) = state {
                        state.release_all(|e| queue.push_back(e));
                    }
                    return 0;
                }
            };
            for record in records.records() {
                let descriptor = if record.kind == Kind::DeviceAdded {
                    record
                        .device
                        .and_then(|number| client.describe(number).ok())
                } else {
                    None
                };
                let mut events = EVENTS.lock();
                let Events { state, queue } = &mut *events;
                if let Some(state) = state {
                    state.feed(record, |_| descriptor, |e| queue.push_back(e));
                }
            }
        }
    })
    .map(drop)
}

pub(crate) fn next(mode: Mode, now: u64) -> Option<KeyEvent> {
    let mut events = EVENTS.lock();
    let Events { state, queue } = &mut *events;
    if let Some(state) = state {
        state.set_mode(mode, |e| queue.push_back(e));
        state.tick(now, |e| queue.push_back(e));
    }
    queue.pop_front()
}

pub(crate) fn controls(mode: Mode) -> Option<Controls> {
    let mut events = EVENTS.lock();
    let Events { state, queue } = &mut *events;
    let state = state.as_mut()?;
    state.set_mode(mode, |e| queue.push_back(e));
    Some(state.controls())
}
