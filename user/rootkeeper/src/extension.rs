//! Порт транзакции расширения композиции принимает операции контракта `Supervisor` в
//! выделенном потоке и передаёт набор обороту supervisor, которому композиция принадлежит.

use alloc::{string::String, sync::Arc, vec::Vec};
use core::num::NonZeroU64;

use ipc::{
    Transport as _,
    wire::{Bytes, Cap, IpcError, Str},
};
use kernel_objects::{MemoryRegion, OwnedHandle, Port, ReceivedFrame, Signal};
use runtime::{Condvar, JoinHandle, Mutex, PortTransport, Priority, Timeout, spawn_owned};
use supervisor_abi::{ExtendError, INSTANCE_NAME_MAX, PLAN_RECORD_MAX, supervisor};
use syscall::{Handle, Rights, SIGNALED, WakeCount};
use userland_image::decode_plan;

use crate::system::Definition;

const SERVING_STACK: u64 = 0x8000;

const SHUTDOWN_TIMEOUT: Timeout = Timeout::from_secs(2);

pub(crate) enum Request {
    Commit {
        caller: NonZeroU64,
        definitions: Vec<Definition>,
    },
    Session {
        caller: NonZeroU64,
        instance: String,
    },
}

pub(crate) enum Answer {
    Extended(Result<(), ExtendError>),
    Session(Result<OwnedHandle, ExtendError>),
}

#[derive(Default)]
struct Exchange {
    request: Option<Request>,
    answer: Option<Answer>,
    stopped: bool,
}

struct Mailbox {
    posted: Signal,
    exchange: Mutex<Exchange>,
    answered: Condvar,
}

struct Staged {
    caller: NonZeroU64,
    definitions: Vec<Definition>,
}

struct Extending {
    mailbox: Arc<Mailbox>,
    staged: Vec<Staged>,
}

struct Serving<'a, 'f> {
    extending: &'a mut Extending,
    received: &'a mut ReceivedFrame<'f>,
    caller: Option<NonZeroU64>,
    vended: Option<OwnedHandle>,
}

pub(crate) struct ExtensionPort {
    root: Option<OwnedHandle>,
    thread: Option<JoinHandle>,
    mailbox: Arc<Mailbox>,
}

impl Mailbox {
    fn create() -> kernel_objects::Result<Self> {
        Ok(Self {
            posted: Signal::create()?,
            exchange: Mutex::new(Exchange::default()),
            answered: Condvar::new(),
        })
    }

    fn ask(&self, request: Request) -> Option<Answer> {
        let mut exchange = self.exchange.lock();
        if exchange.stopped {
            return None;
        }
        exchange.request = Some(request);
        drop(exchange);
        if self.posted.set(SIGNALED, 0, WakeCount::All).is_err() {
            self.exchange.lock().request = None;
            return None;
        }
        let mut exchange = self.exchange.lock();
        while exchange.answer.is_none() && !exchange.stopped {
            exchange = self.answered.wait(exchange);
        }
        exchange.answer.take()
    }

    fn take_request(&self) -> Option<Request> {
        let _ = self.posted.set(0, SIGNALED, WakeCount::None);
        self.exchange.lock().request.take()
    }

    fn answer(&self, answer: Answer) {
        self.exchange.lock().answer = Some(answer);
        self.answered.notify_all();
    }

    fn stop(&self) {
        let mut exchange = self.exchange.lock();
        exchange.stopped = true;
        exchange.request = None;
        drop(exchange);
        self.answered.notify_all();
    }
}

impl Extending {
    fn transaction(&mut self, caller: NonZeroU64) -> Option<&mut Staged> {
        self.staged
            .iter_mut()
            .find(|staged| staged.caller == caller)
    }

    fn discard(&mut self, caller: NonZeroU64) {
        self.staged.retain(|staged| staged.caller != caller);
    }

    fn take(&mut self, caller: NonZeroU64) -> Option<Vec<Definition>> {
        let at = self
            .staged
            .iter()
            .position(|staged| staged.caller == caller)?;
        Some(self.staged.swap_remove(at).definitions)
    }

    fn stage(
        &mut self,
        caller: NonZeroU64,
        record: &[u8],
        program: MemoryRegion,
    ) -> Result<u32, ExtendError> {
        single_component(record)?;
        let staged = self.transaction(caller).ok_or(ExtendError::NoTransaction)?;
        staged.definitions.push(Definition::new(record, program));
        Ok(0)
    }
}

impl Serving<'_, '_> {
    fn vend(&mut self, handle: OwnedHandle) -> Cap {
        let id = handle.as_raw().as_nonzero();
        self.vended = Some(handle);
        Cap::from_raw(id)
    }

    fn end_frame(&mut self, delivered: bool) {
        let Some(handle) = self.vended.take() else {
            return;
        };
        if delivered {
            let _ = handle.into_raw();
        } else {
            let _ = handle.close();
        }
    }
}

impl supervisor::Service for Serving<'_, '_> {
    fn begin(&mut self) -> Result<u32, ExtendError> {
        let caller = self.caller.ok_or(ExtendError::NoTransaction)?;
        self.extending.discard(caller);
        self.extending.staged.push(Staged {
            caller,
            definitions: Vec::new(),
        });
        Ok(0)
    }

    fn define(
        &mut self,
        record: Bytes<'_, PLAN_RECORD_MAX>,
        program: Cap,
    ) -> Result<u32, ExtendError> {
        // Капу региона передаёт принятый кадр, и её перенос закрывает регион на любом
        // отказе операции.
        let program = self
            .received
            .claim(program)
            .map(MemoryRegion::from_handle)
            .ok_or(ExtendError::BadRecord)?;
        let Some(caller) = self.caller else {
            return Err(ExtendError::NoTransaction);
        };
        let staged = self.extending.stage(caller, record.as_bytes(), program);
        if staged.is_err() {
            self.extending.discard(caller);
        }
        staged
    }

    fn commit(&mut self) -> Result<u32, ExtendError> {
        let caller = self.caller.ok_or(ExtendError::NoTransaction)?;
        let definitions = self
            .extending
            .take(caller)
            .ok_or(ExtendError::NoTransaction)?;
        let answered = self.extending.mailbox.ask(Request::Commit {
            caller,
            definitions,
        });
        match answered {
            Some(Answer::Extended(outcome)) => outcome.map(|()| 0),
            _ => Err(ExtendError::NotStarted),
        }
    }

    fn session(&mut self, component: Str<'_, INSTANCE_NAME_MAX>) -> Result<Cap, ExtendError> {
        let caller = self.caller.ok_or(ExtendError::NotFound)?;
        let answered = self.extending.mailbox.ask(Request::Session {
            caller,
            instance: String::from(component.as_str()),
        });
        match answered {
            Some(Answer::Session(Ok(handle))) => Ok(self.vend(handle)),
            Some(Answer::Session(Err(error))) => Err(error),
            _ => Err(ExtendError::NotFound),
        }
    }
}

impl ExtensionPort {
    pub(crate) fn start() -> kernel_objects::Result<Self> {
        let root = Port::create()?.into_handle();
        let server = root.duplicate(Rights::READ, None)?;
        let mailbox = Arc::new(Mailbox::create()?);
        let serving = Arc::clone(&mailbox);
        let thread = spawn_owned(
            move || {
                serve(server, serving);
                0
            },
            SERVING_STACK,
            Priority::new(1),
        )?;
        Ok(Self {
            root: Some(root),
            thread: Some(thread),
            mailbox,
        })
    }

    pub(crate) fn slot(&self, badge: NonZeroU64) -> Option<OwnedHandle> {
        self.root
            .as_ref()?
            .duplicate(Rights::WRITE | Rights::TRANSFER, Some(badge))
            .ok()
    }

    pub(crate) fn posted(&self) -> Handle {
        self.mailbox.posted.handle().as_raw()
    }

    pub(crate) fn take_request(&self) -> Option<Request> {
        self.mailbox.take_request()
    }

    pub(crate) fn answer(&self, answer: Answer) {
        self.mailbox.answer(answer);
    }

    pub(crate) fn shutdown(&mut self) {
        self.mailbox.stop();
        // Закрытие корня отзывает и приёмную капу цикла, и сессии слотов его вызывателей.
        drop(self.root.take());
        let Some(thread) = self.thread.take() else {
            return;
        };
        if thread.wait(SHUTDOWN_TIMEOUT).is_err() {
            let _ = thread.terminate(0);
        }
        let _ = thread.join(SHUTDOWN_TIMEOUT);
    }
}

impl Drop for ExtensionPort {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn serve(server: OwnedHandle, mailbox: Arc<Mailbox>) {
    let transport = PortTransport::server(server.borrow());
    let mut extending = Extending {
        mailbox,
        staged: Vec::new(),
    };
    loop {
        match transport.wait_readable(Timeout::INFINITE) {
            // Приём выше доставил кадр, и его capability, значок и незавершённый ответ
            // принадлежат владеющему кадру до конца обработки.
            Ok(()) => {
                let Some(mut received) = ReceivedFrame::take(&transport) else {
                    continue;
                };
                let mut serving = Serving {
                    caller: NonZeroU64::new(received.badge()),
                    extending: &mut extending,
                    received: &mut received,
                    vended: None,
                };
                let delivered = supervisor::dispatch(&mut serving, &transport).is_ok();
                serving.end_frame(delivered);
            }
            Err(IpcError::SenderGone) => {
                // Уход значка вызывателя снимает его набор вместе с регионами его программ.
                if let Some(gone) = NonZeroU64::new(transport.peer_badge()) {
                    extending.discard(gone);
                }
            }
            Err(IpcError::ResourceExhausted | IpcError::Timeout) => {}
            Err(_) => break,
        }
    }
    drop(server);
}

fn single_component(record: &[u8]) -> Result<(), ExtendError> {
    let plan = decode_plan(record).map_err(|_| ExtendError::BadRecord)?;
    if plan.len() == 1 {
        Ok(())
    } else {
        Err(ExtendError::BadRecord)
    }
}
