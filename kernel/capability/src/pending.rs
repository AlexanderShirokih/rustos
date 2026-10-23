//! Отложенная операция хранит кадр в ядре и сообщает конечный исход через `SIGNALED`.

use alloc::{sync::Arc, vec::Vec};
use core::{
    num::NonZeroU32,
    sync::atomic::{AtomicBool, Ordering},
};

use kbase::{LockCell, MutexCell};
use syscall::{Rights, SIGNALED, decode_tag, encode_tag};

use crate::{
    Capability, DrainedHandles, HandleId, HandleReservation, IpcError, OperationHold, Port, Signal,
    ipc_buffer_xfer::{copy_frame, read_caps, read_tag, release_delivered, write_caps, write_tag},
    port::{OutcomeSlot, RendezvousOutcome, SendKind, ThreadTransport},
    rev_node::revoke_subtree,
    wait::{ParkWaker, Waitable, WakeTarget, Waker},
};

pub(crate) struct StagedCaps {
    pub(crate) caps: Vec<Capability>,
    pub(crate) returns: Vec<HandleReservation>,
}

/// Владеет незавершённой отправкой либо вызовом порта. Ожидается по `SIGNALED`.
/// Закрытие handle отменяет операцию и закрывает неполученные capability.
pub struct Pending {
    port: Arc<Port>,
    pub(crate) kind: SendKind,
    pub(crate) transport: ThreadTransport,
    pub(crate) caps: Arc<MutexCell<StagedCaps>>,
    pub(crate) waker: Arc<ParkWaker>,
    pub(crate) outcome: Arc<OutcomeSlot>,
    done: Arc<Signal>,
    taken: AtomicBool,
}

impl Pending {
    pub(crate) fn completion_signal(&self) -> Arc<Signal> {
        self.done.clone()
    }

    pub(crate) fn start(
        port: &Arc<Port>,
        kind: SendKind,
        sender: &ThreadTransport,
        hold: Option<OperationHold>,
    ) -> Result<HandleId, IpcError> {
        if !port.has_receivers() {
            return Err(IpcError::PeerClosed);
        }
        let transport = ThreadTransport::new_kernel(
            Arc::new(MutexCell::new(syscall::IpcBuffer::zeroed())),
            sender.handle_table.clone(),
        )
        .with_badge(sender.badge);
        copy_frame(sender, &transport)?;
        let count = decode_tag(read_tag(&transport)?).ncaps;
        let ids = read_caps(&transport, count)?
            .into_iter()
            .map(|raw| {
                NonZeroU32::new(raw)
                    .map(HandleId::from_raw)
                    .ok_or(IpcError::BadHandle)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut drained = DrainedHandles::empty();
        let staged = sender.handle_table.with_lock(|table| {
            let reservation = table.reserve_slot()?;
            let result = match kind {
                SendKind::Send => table.stage_for_return(&ids, &mut drained),
                SendKind::Call => table
                    .try_drain_for_transfer(&ids, Rights::TRANSFER, &mut drained)
                    .map(|()| Vec::new()),
            };
            match result {
                Ok(returns) => Ok((reservation, returns)),
                Err(error) => {
                    table.release_reservation(reservation);
                    Err(error)
                }
            }
        });
        let caps = drained.take_handles();
        drained.release();
        let (reservation, returns) = staged?;
        let done = Signal::new();
        let pending = Arc::new(Self {
            port: port.clone(),
            kind,
            transport,
            caps: Arc::new(MutexCell::new(StagedCaps { caps, returns })),
            waker: Arc::new(ParkWaker::new(WakeTarget::Object(done.clone()))),
            outcome: OutcomeSlot::new(),
            done,
            taken: AtomicBool::new(false),
        });
        port.enqueue_pending(&pending);
        drop(hold);
        let mut rejected = DrainedHandles::empty();
        let id = sender.handle_table.with_lock(|table| {
            table.commit_reserved(
                reservation,
                Capability::new_with_default_rights(pending),
                &mut rejected,
            )
        });
        rejected.release();
        Ok(id)
    }

    /// Выдаёт исход один раз и копирует кадр в буфер текущего потока того же процесса.
    /// До сигнала возвращает `ShouldWait`, сохраняя операцию.
    pub fn take(&self, receiver: &ThreadTransport) -> Result<(), IpcError> {
        if self.done.peek() & SIGNALED == 0 {
            return Err(IpcError::ShouldWait);
        }
        if self.taken.swap(true, Ordering::AcqRel) {
            return Err(IpcError::BadHandle);
        }
        let outcome = self.outcome.try_get().map_err(|_| IpcError::Canceled)?;
        let result = match outcome {
            Some(RendezvousOutcome::Delivered) => Ok(()),
            Some(RendezvousOutcome::TransferFailed(error)) => Err(error),
            _ => Err(IpcError::PeerClosed),
        };
        if self.kind == SendKind::Send && result.is_err() {
            let (caps, returns) = self.caps.with_lock(|state| {
                (
                    core::mem::take(&mut state.caps),
                    core::mem::take(&mut state.returns),
                )
            });
            let mut rejected = DrainedHandles::empty();
            let ids = self.transport.handle_table.with_lock(|table| {
                returns
                    .into_iter()
                    .zip(caps)
                    .map(|(slot, cap)| table.commit_reserved(slot, cap, &mut rejected))
                    .collect::<Vec<_>>()
            });
            rejected.release();
            write_caps(&self.transport, &ids)?;
            let len = decode_tag(read_tag(&self.transport)?).len;
            write_tag(&self.transport, encode_tag(len, ids.len()))?;
        } else if self.kind == SendKind::Send || result.is_err() {
            write_tag(&self.transport, encode_tag(0, 0))?;
        }
        if let Err(error) = copy_frame(&self.transport, receiver) {
            let _ = release_delivered(&self.transport);
            return Err(error);
        }
        write_tag(&self.transport, encode_tag(0, 0))?;
        result
    }
}

impl Waitable for Pending {
    fn peek(&self) -> u32 {
        self.done.peek()
    }
    fn register_waiter(&self, mask: u32, waker: Arc<dyn Waker>) {
        self.done.register_waiter(mask, waker);
    }
    fn remove_waiter(&self, waker: &Arc<dyn Waker>) -> bool {
        self.done.remove_waiter(waker)
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if !self.taken.load(Ordering::Acquire) {
            if !self.port.remove_waiter(&self.waker) {
                self.outcome.cancel_after_dequeue();
            }
            if self.kind == SendKind::Call && self.outcome.raw() == OutcomeSlot::DELIVERED {
                let _ = release_delivered(&self.transport);
            }
        }
        let (caps, returns) = self.caps.with_lock(|state| {
            (
                core::mem::take(&mut state.caps),
                core::mem::take(&mut state.returns),
            )
        });
        self.transport.handle_table.with_lock(|table| {
            for slot in returns {
                table.release_reservation(slot);
            }
        });
        for cap in &caps {
            revoke_subtree(cap.node());
        }
        drop(caps);
    }
}
