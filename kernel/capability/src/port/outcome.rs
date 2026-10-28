//! Исход рандеву и его атомарный слот-арбитр.
//!
//! [`OutcomeSlot`] - общая с вызывателем ячейка состояния. Матчер, reply и
//! тайм-аут соревнуются CAS-переходами, и побеждает ровно одна сторона.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::IpcError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SendKind {
    Send,
    Call,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RendezvousOutcome {
    Delivered,
    PeerGone,
    TransferFailed(IpcError),
    SenderGone(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InvalidTransferError(u32);

pub(crate) struct OutcomeSlot {
    state: AtomicU32,
    badge: AtomicU64,
    transfer_error: AtomicU32,
    #[cfg(test)]
    cancel_waiters: AtomicU32,
}

impl OutcomeSlot {
    pub(crate) const PENDING: u32 = 0;
    pub(crate) const AWAITING_REPLY: u32 = 1;
    pub(crate) const DELIVERING: u32 = 2;
    pub(crate) const DELIVERED: u32 = 3;
    pub(crate) const PEER_GONE: u32 = 4;
    pub(crate) const TRANSFER_FAILED: u32 = 5;
    pub(crate) const TIMEDOUT: u32 = 6;
    pub(crate) const SENDER_GONE: u32 = 7;
    pub(crate) const CANCELED: u32 = 8;

    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: AtomicU32::new(Self::PENDING),
            badge: AtomicU64::new(0),
            transfer_error: AtomicU32::new(0),
            #[cfg(test)]
            cancel_waiters: AtomicU32::new(0),
        })
    }

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
            RendezvousOutcome::TransferFailed(error) => {
                self.transfer_error
                    .store(encode_transfer_error(error), Ordering::Relaxed);
                Self::TRANSFER_FAILED
            }
            RendezvousOutcome::SenderGone(badge) => {
                self.badge.store(badge, Ordering::Release);
                Self::SENDER_GONE
            }
        };
        self.state.store(v, Ordering::Release);
    }

    pub(crate) fn try_get(&self) -> Result<Option<RendezvousOutcome>, InvalidTransferError> {
        let outcome = match self.state.load(Ordering::Acquire) {
            Self::DELIVERED => Some(RendezvousOutcome::Delivered),
            Self::PEER_GONE => Some(RendezvousOutcome::PeerGone),
            Self::TRANSFER_FAILED => Some(RendezvousOutcome::TransferFailed(
                decode_transfer_error(self.transfer_error.load(Ordering::Relaxed))?,
            )),
            Self::SENDER_GONE => Some(RendezvousOutcome::SenderGone(
                self.badge.load(Ordering::Acquire),
            )),
            _ => None,
        };
        Ok(outcome)
    }

    #[cfg(test)]
    pub(crate) fn get(&self) -> Option<RendezvousOutcome> {
        self.try_get()
            .expect("outcome holds a valid transfer error")
    }

    pub(crate) fn raw(&self) -> u32 {
        self.state.load(Ordering::Acquire)
    }

    pub(crate) fn set_awaiting_reply(&self) {
        self.state.store(Self::AWAITING_REPLY, Ordering::Release);
    }

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

    pub(crate) fn cancel_queued(&self) {
        let _ = self.state.compare_exchange(
            Self::PENDING,
            Self::CANCELED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    fn try_cancel_reply(&self) -> bool {
        self.state
            .compare_exchange(
                Self::AWAITING_REPLY,
                Self::CANCELED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(crate) fn cancel_after_dequeue(&self) {
        #[cfg(test)]
        self.cancel_waiters.fetch_add(1, Ordering::AcqRel);
        loop {
            match self.raw() {
                // Перенос уже закоммичен и держит IPC-буфер. Отмена не обрывает его, а ждёт
                // терминального исхода.
                Self::DELIVERING => core::hint::spin_loop(),
                Self::AWAITING_REPLY => {
                    if self.try_cancel_reply() {
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

const TRANSFER_ERRORS: [IpcError; 13] = [
    IpcError::BadHandle,
    IpcError::WrongType,
    IpcError::AccessDenied,
    IpcError::ShouldWait,
    IpcError::PeerClosed,
    IpcError::SenderGone,
    IpcError::Timeout,
    IpcError::Canceled,
    IpcError::BufferTooSmall,
    IpcError::MessageTooBig,
    IpcError::OutOfHandles,
    IpcError::ResourceExhausted,
    IpcError::Revoked,
];

fn encode_transfer_error(error: IpcError) -> u32 {
    error as u32
}

fn decode_transfer_error(raw: u32) -> Result<IpcError, InvalidTransferError> {
    usize::try_from(raw)
        .ok()
        .and_then(|index| TRANSFER_ERRORS.get(index))
        .copied()
        .ok_or(InvalidTransferError(raw))
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc as StdArc, Barrier},
        thread,
    };

    use super::{
        InvalidTransferError, OutcomeSlot, RendezvousOutcome, TRANSFER_ERRORS,
        decode_transfer_error,
    };

    #[test]
    fn transfer_failure_preserves_its_exact_error() {
        for error in TRANSFER_ERRORS {
            let outcome = OutcomeSlot::new();
            outcome.set(RendezvousOutcome::TransferFailed(error));
            assert_eq!(
                outcome.get(),
                Some(RendezvousOutcome::TransferFailed(error))
            );
            assert_eq!(decode_transfer_error(error as u32), Ok(error));
        }
        assert_eq!(
            decode_transfer_error(u32::MAX),
            Err(InvalidTransferError(u32::MAX))
        );
    }

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
