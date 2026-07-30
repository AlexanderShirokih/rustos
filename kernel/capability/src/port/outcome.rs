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
}

impl OutcomeSlot {
    pub(crate) const PENDING: u32 = 0;
    /// `call`: запрос доставлен серверу, вызывающая сторона ждёт reply.
    pub(crate) const AWAITING_REPLY: u32 = 1;
    /// Перенос ответа в процессе: вызывающая сторона обязан дождаться
    /// терминального исхода, не уходя по тайм-ауту.
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
        })
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
}
