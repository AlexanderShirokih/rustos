//! `Reply` kernel object: одноразовый ответный канал рандеву-`call`.
//!
//! Когда поток делает `call` на [`Port`](super::port::Port),
//! ядро создаёт `Reply`, держащий идентификацию заблокированного вызывателя.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use collections::{LockCell, MutexCell};

use super::{
    errors::IpcError,
    ipc_buffer_xfer::transfer_rendezvous,
    port::{OutcomeSlot, RendezvousOutcome, ThreadTransport},
    wait::ParkWaker,
};

/// Одноразовый ответный объект на заблокированного вызывателя `call`.
pub struct Reply {
    /// Транспорт вызывателя (mapper его AS, VA IPC-буфера, handle-table).
    caller: ThreadTransport,
    /// Park-waker вызывателя - тот же, на котором он заблокирован в `call`.
    waker: Arc<ParkWaker>,
    /// Outcome-слот вызывателя: сюда пишется исход reply.
    outcome: Arc<OutcomeSlot>,
    /// `true` после использования или инвалидизации.
    used: AtomicBool,
}

impl Reply {
    pub(crate) fn new(
        caller: ThreadTransport,
        waker: Arc<ParkWaker>,
        outcome: Arc<OutcomeSlot>,
    ) -> Arc<Self> {
        Arc::new(Self {
            caller,
            waker,
            outcome,
            used: AtomicBool::new(false),
        })
    }

    /// Доставляет ответ из IPC-буфера сервера (`server`) в IPC-буфер
    /// вызывателя, переносит caps, будит вызывателя и инвалидирует Reply.
    ///
    /// Повторный `reply` на том же объекте вернёт [`IpcError::BadHandle`].
    /// На провале переноса вызывающая сторона будится с `TransferFailed`.
    pub fn reply(&self, server: &ThreadTransport) -> Result<(), IpcError> {
        if self
            .used
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(IpcError::BadHandle);
        }

        // Коммитим доставку под общим с вызывателем арбитром.
        if !self.outcome.try_begin_reply() {
            return Err(IpcError::PeerClosed);
        }

        // server = отправитель ответа, caller = получатель ответа.
        match transfer_rendezvous(server, &self.caller) {
            Ok(()) => {
                self.outcome.set(RendezvousOutcome::Delivered);
                self.waker.signal_match();
                Ok(())
            }
            Err(e) => {
                self.outcome.set(RendezvousOutcome::TransferFailed);
                self.waker.signal_match();
                Err(e)
            }
        }
    }

    /// Инвалидирует Reply без ответа: будит вызывателя с `PeerGone`.
    /// No-op, если Reply уже использован.
    pub fn cancel(&self) {
        use super::wait::CancelTarget;
        if self
            .used
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            if self.outcome.try_begin_reply() {
                self.outcome.set(RendezvousOutcome::PeerGone);
                self.waker.cancel();
            }
        }
    }
}

impl Drop for Reply {
    fn drop(&mut self) {
        // Закрытие последнего хендла на Reply без ответа не должно оставлять
        // вызывателя висеть: будим его с PeerGone.
        let used = self.used.load(Ordering::Acquire);
        if !used {
            use super::wait::CancelTarget;
            if self
                .used
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
                && self.outcome.try_begin_reply()
            {
                self.outcome.set(RendezvousOutcome::PeerGone);
                self.waker.cancel();
            }
        }
    }
}

/// Одноразовый ящик для Reply, передаваемого матчером проснувшемуся
/// recv-получателю. Матчер `install`'ит, получатель `take`'ает.
pub struct ReplySlot {
    inner: MutexCell<Option<Arc<Reply>>>,
}

impl ReplySlot {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: MutexCell::new(None),
        })
    }

    pub(crate) fn install(&self, reply: Arc<Reply>) {
        self.inner.with_lock(|slot| *slot = Some(reply));
    }

    /// Забирает Reply (если был положен матчером).
    pub fn take(&self) -> Option<Arc<Reply>> {
        self.inner.with_lock(Option::take)
    }
}
