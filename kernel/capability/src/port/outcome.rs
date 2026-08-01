//! Исход рандеву и его атомарный слот-арбитр.
//!
//! [`OutcomeSlot`] - общая с вызывателем ячейка состояния: матчер, reply и
//! тайм-аут соревнуются CAS-переходами, побеждает ровно одна сторона.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Роль стороны, стоящей в очереди отправителей.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SendKind {
    /// `send`: после доставки отправитель разблокируется немедленно.
    Send,
    /// `call`: вызывающая сторона остаётся блокированной до `reply`.
    Call,
}

/// Исход операции, прочитанный разбуженной стороной из [`OutcomeSlot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RendezvousOutcome {
    /// Встреча состоялась, сообщение доставлено.
    Delivered,
    /// Port/Reply уничтожен или последний хендл закрыт.
    PeerGone,
    /// Перенос caps или копирование тела провалились.
    TransferFailed,
    /// Отправители значка исчезли: кадра нет, получателю доставлен значок.
    SenderGone(u64),
}

/// Атомарный слот исхода рандеву.
pub(crate) struct OutcomeSlot {
    state: AtomicU32,
    badge: AtomicU64,
    #[cfg(test)]
    cancel_waiters: AtomicU32,
}

impl OutcomeSlot {
    pub(crate) const PENDING: u32 = 0;
    /// `call`: запрос доставлен серверу, вызывающая сторона ждёт reply.
    pub(crate) const AWAITING_REPLY: u32 = 1;
    /// Matcher или reply уже изъял transport и выполняет перенос.
    /// Владелец IPC-буфера обязан дождаться терминального исхода.
    pub(crate) const DELIVERING: u32 = 2;
    pub(crate) const DELIVERED: u32 = 3;
    pub(crate) const PEER_GONE: u32 = 4;
    pub(crate) const TRANSFER_FAILED: u32 = 5;
    /// Вызывающая сторона ушла по тайм-ауту до reply.
    pub(crate) const TIMEDOUT: u32 = 6;
    /// Отправители значка исчезли; значок лежит в `badge`.
    pub(crate) const SENDER_GONE: u32 = 7;

    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: AtomicU32::new(Self::PENDING),
            badge: AtomicU64::new(0),
            #[cfg(test)]
            cancel_waiters: AtomicU32::new(0),
        })
    }

    /// Matcher под Port-локом линеаризует dequeue и сохраняет
    /// transport до публикации исхода переноса.
    pub(crate) fn begin_delivery(&self) {
        let previous = self.state.compare_exchange(
            Self::PENDING,
            Self::DELIVERING,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        debug_assert!(previous.is_ok(), "delivery started from non-pending state");
    }

    pub(crate) fn set(&self, outcome: RendezvousOutcome) {
        let v = match outcome {
            RendezvousOutcome::Delivered => Self::DELIVERED,
            RendezvousOutcome::PeerGone => Self::PEER_GONE,
            RendezvousOutcome::TransferFailed => Self::TRANSFER_FAILED,
            RendezvousOutcome::SenderGone(badge) => {
                self.badge.store(badge, Ordering::Release);
                Self::SENDER_GONE
            }
        };
        self.state.store(v, Ordering::Release);
    }

    /// Терминальный исход; `None` для нетерминальных состояний
    /// (`PENDING`/`AWAITING_REPLY`/`DELIVERING`/`TIMEDOUT`).
    pub fn get(&self) -> Option<RendezvousOutcome> {
        match self.state.load(Ordering::Acquire) {
            Self::DELIVERED => Some(RendezvousOutcome::Delivered),
            Self::PEER_GONE => Some(RendezvousOutcome::PeerGone),
            Self::TRANSFER_FAILED => Some(RendezvousOutcome::TransferFailed),
            Self::SENDER_GONE => Some(RendezvousOutcome::SenderGone(
                self.badge.load(Ordering::Acquire),
            )),
            _ => None,
        }
    }

    /// Сырое состояние слота.
    pub(crate) fn raw(&self) -> u32 {
        self.state.load(Ordering::Acquire)
    }

    /// `call`-матчер: запрос доставлен, переводим вызывающую сторону в ожидание reply.
    pub(crate) fn set_awaiting_reply(&self) {
        self.state.store(Self::AWAITING_REPLY, Ordering::Release);
    }

    /// Сервер-reply коммитит доставку ответа: `AWAITING_REPLY -> DELIVERING`.
    /// `true`, если выиграл гонку с тайм-аутом вызывателя.
    pub(crate) fn try_begin_reply(&self) -> bool {
        self.state
            .compare_exchange(
                Self::AWAITING_REPLY,
                Self::DELIVERING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Вызывающая сторона уходит по тайм-ауту ожидания reply: `AWAITING_REPLY ->
    /// TIMEDOUT`. `true`, если выиграл гонку с reply.
    pub(crate) fn try_timeout(&self) -> bool {
        self.state
            .compare_exchange(
                Self::AWAITING_REPLY,
                Self::TIMEDOUT,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// External cancel после dequeue: либо отменяет ожидание reply,
    /// либо дожидается завершения уже закоммиченного переноса.
    pub(crate) fn cancel_after_dequeue(&self) {
        #[cfg(test)]
        self.cancel_waiters.fetch_add(1, Ordering::AcqRel);
        loop {
            match self.raw() {
                Self::DELIVERING => core::hint::spin_loop(),
                Self::AWAITING_REPLY => {
                    if self.try_timeout() {
                        break;
                    }
                }
                _ => break,
            }
        }
        #[cfg(test)]
        self.cancel_waiters.fetch_sub(1, Ordering::AcqRel);
    }

    #[cfg(test)]
    pub(crate) fn cancel_waiter_count(&self) -> u32 {
        self.cancel_waiters.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc as StdArc, Barrier},
        thread,
    };

    use super::OutcomeSlot;

    #[test]
    fn race_reply_and_timeout_choose_one_terminal_path() {
        for _ in 0..128 {
            let outcome = OutcomeSlot::new();
            outcome.set_awaiting_reply();
            let barrier = StdArc::new(Barrier::new(3));

            let reply_outcome = outcome.clone();
            let reply_barrier = barrier.clone();
            let reply = thread::spawn(move || {
                reply_barrier.wait();
                reply_outcome.try_begin_reply()
            });

            let timeout_outcome = outcome.clone();
            let timeout_barrier = barrier.clone();
            let timeout = thread::spawn(move || {
                timeout_barrier.wait();
                timeout_outcome.try_timeout()
            });

            barrier.wait();
            let reply_won = reply.join().expect("reply contender joins");
            let timeout_won = timeout.join().expect("timeout contender joins");

            assert_ne!(reply_won, timeout_won);
            let expected = if reply_won {
                OutcomeSlot::DELIVERING
            } else {
                OutcomeSlot::TIMEDOUT
            };
            assert_eq!(outcome.raw(), expected);
        }
    }
}
