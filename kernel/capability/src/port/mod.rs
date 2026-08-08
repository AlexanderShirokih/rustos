//! `Port` - объект без очереди сообщений в ядре. Внутри он держит очередь ждущих потоков:
//! либо отправителей, либо получателей - но никогда оба одновременно.
//!
//! Транспорт сообщения - per-thread IPC-буфер. Тело копируется
//! напрямую между IPC-буферами двух потоков через линейное физическое отображение.
//! Хендлы переносятся кросс-процессно через handle-таблицы
//! (см. [`ipc_buffer_xfer`](super::ipc_buffer_xfer)).
//!
//! Парковка/пробуждение используют [`ParkWaker`](super::wait): CAS на
//! `state` (REGISTERED -> SIGNALED/CANCELED) выбирает ровно одного победителя
//! в гонке match/cancel.
//!
//! Порт считает своих держателей: получателей (capability с правом `READ`) и отправителей каждого
//! значка (capability со значком и правом `WRITE`). Учёт живёт под локом очереди, поэтому
//! изменение множества и разбор очереди атомарны. Операция удерживает учёт своей стороны от
//! снимка capability до доставки или постановки в очередь ([`OperationHold`]).

mod outcome;
mod state;
mod transport;

use alloc::{sync::Arc, vec::Vec};
use core::{
    num::NonZeroU64,
    sync::atomic::{AtomicBool, Ordering},
};

use collections::{LockCell, MutexCell};
use syscall::Rights;

#[cfg(test)]
use self::state::DEPARTED_BADGE_LIMIT;
use self::state::{PortState, ReceiverWaiter, RecvPick, SenderWaiter, Waiter};
pub use self::transport::{KernelIpcBuffer, ThreadTransport};
pub(crate) use self::{
    outcome::{OutcomeSlot, RendezvousOutcome, SendKind},
    transport::BufferAccess,
};
use super::{
    errors::IpcError,
    ipc_buffer_xfer::{transfer_rendezvous, write_badge},
    reply::{Reply, ReplySlot},
    rev_node::RevocationHook,
    runtime::{KernelRuntime, ParkState},
    target::{HolderTicket, OperationHold},
    wait::{CancelTarget, ParkWaker, deadline_after, park_until_deadline, remaining_until},
};

/// Port - объект, реализующий синхронный обмен сообщениями.
pub struct Port {
    inner: MutexCell<PortState>,
}

struct PortHolder {
    port: Arc<Port>,
    receiver: bool,
    badge: Option<NonZeroU64>,
    counted: AtomicBool,
    released: AtomicBool,
}

impl HolderTicket for PortHolder {
    fn activate(&self) -> Result<(), IpcError> {
        self.port.activate_holder(self)
    }

    fn release(&self) {
        if self.released.swap(true, Ordering::AcqRel) {
            return;
        }
        self.port.release_holder(self);
    }

    fn reacquire(&self) -> Option<Arc<dyn HolderTicket>> {
        self.port.reacquire_holder(self)
    }
}

impl RevocationHook for PortHolder {
    fn revoke(&self) {
        self.release();
    }
}

/// Снимок port-capability для одной операции: объект, значок хендла и удержание учёта его стороны.
pub struct PortAccess {
    pub port: Arc<Port>,
    pub badge: u64,
    pub hold: Option<OperationHold>,
}

pub(crate) struct ParkCtx {
    waker: Arc<ParkWaker>,
    outcome: Arc<OutcomeSlot>,
}

impl ParkCtx {
    pub(crate) fn for_current(runtime: &Arc<dyn KernelRuntime>) -> Self {
        // Вызов идёт вне queue-лока: `current_wait_token` берёт scheduler-лок, а обратное
        // ребро занято терминацией потока (`PortCancel`).
        let token = runtime.current_wait_token();
        Self {
            waker: Arc::new(ParkWaker::new(runtime.clone(), token)),
            outcome: OutcomeSlot::new(),
        }
    }
}

pub(crate) enum SendAction {
    Park {
        waker: Arc<ParkWaker>,
        outcome: Arc<OutcomeSlot>,
    },
    Done,
    Failed(IpcError),
}

pub(crate) enum CallAction {
    Park {
        waker: Arc<ParkWaker>,
        outcome: Arc<OutcomeSlot>,
    },
    Failed(IpcError),
}

pub(crate) enum RecvAction {
    Park {
        waker: Arc<ParkWaker>,
        outcome: Arc<OutcomeSlot>,
        reply_slot: Arc<ReplySlot>,
    },
    Done {
        reply: Option<Arc<Reply>>,
    },
    SenderGone {
        badge: u64,
    },
    Failed(IpcError),
}

impl Port {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: MutexCell::new(PortState::new()),
        })
    }

    pub(super) fn holder_ticket(self: &Arc<Self>, rights: Rights) -> Option<Arc<dyn HolderTicket>> {
        rights
            .contains(Rights::READ)
            .then(|| self.make_ticket(true, None, false))
    }

    pub(super) fn counted_holder_ticket(
        self: &Arc<Self>,
        rights: Rights,
    ) -> Option<Arc<dyn HolderTicket>> {
        if !rights.contains(Rights::READ) {
            return None;
        }
        self.inner.with_lock(PortState::add_receiver);
        Some(self.make_ticket(true, None, true))
    }

    pub(super) fn badged_holder_ticket(
        self: &Arc<Self>,
        rights: Rights,
        badge: NonZeroU64,
    ) -> Option<Arc<dyn HolderTicket>> {
        if !rights.contains(Rights::WRITE) {
            return self.holder_ticket(rights);
        }
        Some(self.make_ticket(rights.contains(Rights::READ), Some(badge), false))
    }

    fn make_ticket(
        self: &Arc<Self>,
        receiver: bool,
        badge: Option<NonZeroU64>,
        counted: bool,
    ) -> Arc<dyn HolderTicket> {
        Arc::new(PortHolder {
            port: self.clone(),
            receiver,
            badge,
            counted: AtomicBool::new(counted),
            released: AtomicBool::new(false),
        })
    }

    fn activate_holder(&self, holder: &PortHolder) -> Result<(), IpcError> {
        self.inner.with_lock(|state| {
            // Билет, отозванный между созданием и активацией, отвергается без следов в учёте.
            if holder.released.load(Ordering::Acquire) {
                return Err(IpcError::Revoked);
            }
            // Значок учитывается до получателя: его отказ оставляет множество получателей
            // нетронутым.
            if let Some(badge) = holder.badge {
                state.register_badge(badge.get())?;
            }
            if holder.receiver {
                state.add_receiver();
            }
            holder.counted.store(true, Ordering::Release);
            Ok(())
        })
    }

    fn reacquire_holder(self: &Arc<Self>, holder: &PortHolder) -> Option<Arc<dyn HolderTicket>> {
        let counted = self.inner.with_lock(|state| {
            // Снятый учёт вторым токеном не оживает: `release` выставляет флаг до захвата
            // лока, поэтому сброшенный под локом флаг означает живого держателя.
            if holder.released.load(Ordering::Acquire) {
                return false;
            }
            // Запись живого значка существует: в ней учтён как минимум сам холдер.
            if let Some(badge) = holder.badge
                && state.register_badge(badge.get()).is_err()
            {
                return false;
            }
            if holder.receiver {
                state.add_receiver();
            }
            true
        });
        counted.then(|| self.make_ticket(holder.receiver, holder.badge, true))
    }

    fn release_holder(&self, holder: &PortHolder) {
        let wakes = self.inner.with_lock(|state| {
            let mut wakes = Vec::new();
            if !holder.counted.load(Ordering::Acquire) {
                return wakes;
            }
            if holder.receiver {
                state.release_receiver(&mut wakes);
            }
            if let Some(badge) = holder.badge {
                state.release_badge(badge.get(), &mut wakes);
            }
            wakes
        });
        for wake in wakes {
            wake.wake();
        }
    }

    pub(crate) fn send(
        &self,
        sender: &ThreadTransport,
        runtime: &Arc<dyn KernelRuntime>,
        hold: Option<OperationHold>,
    ) -> SendAction {
        let action = loop {
            match self.inner.with_lock(PortState::take_receiver) {
                Err(e) => break SendAction::Failed(e),
                Ok(Some(receiver)) => break deliver_send(sender, &receiver),
                Ok(None) => {}
            }

            // Матч и постановка в очередь - разные захваты queue-лока: между ними берётся
            // wait-token. Изменившееся в это окно состояние отвергает постановку, и цикл
            // повторяет матч.
            let ctx = ParkCtx::for_current(runtime);
            let waiter = Waiter {
                kind: SendKind::Send,
                transport: sender.clone(),
                waker: ctx.waker.clone(),
                outcome: ctx.outcome.clone(),
                reply_slot: ReplySlot::new(),
            };
            if self
                .inner
                .with_lock(|state| state.try_push_sender(waiter))
                .is_ok()
            {
                break SendAction::Park {
                    waker: ctx.waker,
                    outcome: ctx.outcome,
                };
            }
        };
        // Уход значка застаёт кадр доставленным либо waiter в очереди.
        drop(hold);
        action
    }

    pub(crate) fn call(
        &self,
        caller: &ThreadTransport,
        ctx: ParkCtx,
        hold: Option<OperationHold>,
    ) -> CallAction {
        let action = loop {
            match self.inner.with_lock(PortState::take_receiver) {
                Err(e) => break CallAction::Failed(e),
                Ok(Some(receiver)) => break deliver_call(caller, &receiver, ctx),
                Ok(None) => {}
            }

            let waiter = Waiter {
                kind: SendKind::Call,
                transport: caller.clone(),
                waker: ctx.waker.clone(),
                outcome: ctx.outcome.clone(),
                reply_slot: ReplySlot::new(),
            };
            if self
                .inner
                .with_lock(|state| state.try_push_sender(waiter))
                .is_ok()
            {
                break CallAction::Park {
                    waker: ctx.waker,
                    outcome: ctx.outcome,
                };
            }
        };
        drop(hold);
        action
    }

    pub(crate) fn recv(
        &self,
        receiver: &ThreadTransport,
        runtime: &Arc<dyn KernelRuntime>,
        hold: Option<OperationHold>,
    ) -> RecvAction {
        let action = loop {
            match self.inner.with_lock(PortState::pick_for_receiver) {
                RecvPick::DepartedBadge(badge) => break RecvAction::SenderGone { badge },
                RecvPick::Sender(sender) => break deliver_recv(sender, receiver),
                RecvPick::Nothing => {}
            }

            let ctx = ParkCtx::for_current(runtime);
            let reply_slot = ReplySlot::new();
            let waiter = Waiter {
                kind: (),
                transport: receiver.clone(),
                waker: ctx.waker.clone(),
                outcome: ctx.outcome.clone(),
                reply_slot: reply_slot.clone(),
            };
            if self
                .inner
                .with_lock(|state| state.try_push_receiver(waiter))
                .is_ok()
            {
                break RecvAction::Park {
                    waker: ctx.waker,
                    outcome: ctx.outcome,
                    reply_slot,
                };
            }
        };
        // Обнуление множества получателей застаёт приём завершённым либо waiter в очереди.
        drop(hold);
        action
    }

    /// Будит всех ждущих с исходом `PeerGone`.
    pub fn cancel_all(&self) {
        let wakes = self.inner.with_lock(|state| {
            let mut wakes = Vec::new();
            state.break_all(&mut wakes);
            wakes
        });
        for wake in wakes {
            wake.wake();
        }
    }

    pub(crate) fn remove_waiter(&self, waker: &Arc<ParkWaker>) -> bool {
        // Waiter не роняется под локом порта: его транспорт держит capability-таблицу.
        let removed = self.inner.with_lock(|state| state.take_by_waker(waker));
        removed.is_some()
    }

    #[cfg(test)]
    pub(crate) fn queue_is_empty(&self) -> bool {
        self.inner.with_lock(|state| state.queue_is_empty())
    }
}

fn deliver_send(sender: &ThreadTransport, receiver: &ReceiverWaiter) -> SendAction {
    match transfer_rendezvous(sender, &receiver.transport) {
        Ok(()) => {
            receiver.outcome.set(RendezvousOutcome::Delivered);
            receiver.waker.signal_match();
            SendAction::Done
        }
        Err(e) => {
            receiver.outcome.set(RendezvousOutcome::TransferFailed(e));
            receiver.waker.signal_match();
            SendAction::Failed(e)
        }
    }
}

fn deliver_call(caller: &ThreadTransport, receiver: &ReceiverWaiter, ctx: ParkCtx) -> CallAction {
    match transfer_rendezvous(caller, &receiver.transport) {
        Ok(()) => {
            ctx.outcome.set_awaiting_reply();
            let reply = Reply::new(caller.clone(), ctx.waker.clone(), ctx.outcome.clone());
            receiver.reply_slot.install(reply);
            receiver.outcome.set(RendezvousOutcome::Delivered);
            receiver.waker.signal_match();
            CallAction::Park {
                waker: ctx.waker,
                outcome: ctx.outcome,
            }
        }
        Err(e) => {
            receiver.outcome.set(RendezvousOutcome::TransferFailed(e));
            receiver.waker.signal_match();
            CallAction::Failed(e)
        }
    }
}

fn deliver_recv(sender: SenderWaiter, receiver: &ThreadTransport) -> RecvAction {
    match transfer_rendezvous(&sender.transport, receiver) {
        Ok(()) => match sender.kind {
            SendKind::Send => {
                sender.outcome.set(RendezvousOutcome::Delivered);
                sender.waker.signal_match();
                RecvAction::Done { reply: None }
            }
            SendKind::Call => {
                sender.outcome.set_awaiting_reply();
                let reply = Reply::new(sender.transport, sender.waker, sender.outcome);
                RecvAction::Done { reply: Some(reply) }
            }
        },
        Err(e) => {
            sender.outcome.set(RendezvousOutcome::TransferFailed(e));
            sender.waker.signal_match();
            RecvAction::Failed(e)
        }
    }
}

fn outcome_to_result(outcome: &Arc<OutcomeSlot>) -> Result<(), IpcError> {
    match outcome.try_get() {
        Ok(Some(RendezvousOutcome::Delivered)) => Ok(()),
        Ok(Some(RendezvousOutcome::PeerGone)) => Err(IpcError::PeerClosed),
        Ok(Some(RendezvousOutcome::TransferFailed(error))) => Err(error),
        Ok(Some(RendezvousOutcome::SenderGone(_))) => Err(IpcError::SenderGone),
        // Парковка завершилась без публикации исхода - трактуем как Canceled.
        Ok(None) | Err(_) => Err(IpcError::Canceled),
    }
}

const EXPIRED_RECHECK_NS: u64 = 1_000_000;

fn wait_resolved(runtime: &Arc<dyn KernelRuntime>, waker: &Arc<ParkWaker>) {
    while waker.state().load(Ordering::Acquire) == ParkState::REGISTERED {
        runtime.block_current_until(waker.state(), None);
    }
}

fn resolve_send_recv(
    port: &Arc<Port>,
    runtime: &Arc<dyn KernelRuntime>,
    waker: &Arc<ParkWaker>,
    outcome: &Arc<OutcomeSlot>,
    deadline_ns: Option<u64>,
) -> Result<(), IpcError> {
    park_until_deadline(runtime, waker, deadline_ns);
    if waker.state().load(Ordering::Acquire) == ParkState::REGISTERED {
        if port.remove_waiter(waker) {
            waker.claim_timeout();
            return Err(IpcError::Timeout);
        }
        // Waiter забран матчером: исход публикуется, дожидаемся сигнала.
        wait_resolved(runtime, waker);
    }
    outcome_to_result(outcome)
}

/// Реализует send из IPC-буфера отправителя. `hold` - удержание учёта его стороны, снимаемое после
/// доставки или постановки в очередь.
pub fn port_send(
    port: &Arc<Port>,
    sender: &ThreadTransport,
    runtime: &Arc<dyn KernelRuntime>,
    timeout_ns: Option<u64>,
    hold: Option<OperationHold>,
) -> Result<(), IpcError> {
    let deadline_ns = deadline_after(runtime, timeout_ns);
    match port.send(sender, runtime, hold) {
        SendAction::Done => Ok(()),
        SendAction::Failed(e) => Err(e),
        SendAction::Park { waker, outcome } => {
            arm_port_cancel(port, runtime, &waker, &outcome, None);
            let result = resolve_send_recv(port, runtime, &waker, &outcome, deadline_ns);
            runtime.clear_blocked_cancel();
            result
        }
    }
}

/// Реализует call, блокирующий поток до reply. Ответ оказывается в IPC-буфере вызывателя.
/// `timeout_ns` ограничивает всю операцию (ожидание получателя + ожидание
/// reply);
/// семантика значений и `hold` - как у [`port_send`].
/// На истечении - [`IpcError::Timeout`].
pub fn port_call(
    port: &Arc<Port>,
    caller: &ThreadTransport,
    runtime: &Arc<dyn KernelRuntime>,
    timeout_ns: Option<u64>,
    hold: Option<OperationHold>,
) -> Result<(), IpcError> {
    let deadline_ns = deadline_after(runtime, timeout_ns);
    // вызывающая сторона всегда паркуется до reply - предсоздаём park-ctx, чтобы
    // Reply мог на него ссылаться даже на синхронном матче.
    match port.call(caller, ParkCtx::for_current(runtime), hold) {
        CallAction::Failed(e) => Err(e),
        CallAction::Park { waker, outcome } => {
            arm_port_cancel(port, runtime, &waker, &outcome, None);
            let result = resolve_call(port, runtime, &waker, &outcome, deadline_ns);
            runtime.clear_blocked_cancel();
            result
        }
    }
}

fn resolve_call(
    port: &Arc<Port>,
    runtime: &Arc<dyn KernelRuntime>,
    waker: &Arc<ParkWaker>,
    outcome: &Arc<OutcomeSlot>,
    deadline_ns: Option<u64>,
) -> Result<(), IpcError> {
    loop {
        match outcome.raw() {
            OutcomeSlot::PENDING | OutcomeSlot::DELIVERING => {
                let park_ns = match remaining_until(runtime, deadline_ns) {
                    // Из очереди уводит истёкший дедлайн; в переносе уйти нельзя и на
                    // нём, поэтому парковка перевзводится коротким шагом.
                    Some(0) => {
                        if port.remove_waiter(waker) {
                            waker.claim_timeout();
                            return Err(IpcError::Timeout);
                        }
                        Some(EXPIRED_RECHECK_NS)
                    }
                    remaining => remaining,
                };
                runtime.block_current_until(waker.state(), park_ns);
            }
            OutcomeSlot::AWAITING_REPLY => match remaining_until(runtime, deadline_ns) {
                Some(0) => {
                    if outcome.try_timeout() {
                        waker.claim_timeout();
                        return Err(IpcError::Timeout);
                    }
                    // Иначе сервер закоммитил reply (DELIVERING) - дочитываем.
                }
                // Разбужены до дедлайна - досыпаем остаток, ранний Timeout запрещён.
                remaining => runtime.block_current_until(waker.state(), remaining),
            },
            OutcomeSlot::TIMEDOUT => return Err(IpcError::Timeout),
            _ => return outcome_to_result(outcome),
        }
    }
}

/// `recv`: блокирующая. Возвращает `Some(reply)`, если встречным был `call`. Семантика
/// `timeout_ns` и `hold` - как у [`port_send`], на истечении - [`IpcError::Timeout`].
/// [`IpcError::SenderGone`] - кадра нет, а в `IpcBuffer.badge` лежит значок, чьи отправители
/// исчезли.
pub fn port_recv(
    port: &Arc<Port>,
    receiver: &ThreadTransport,
    runtime: &Arc<dyn KernelRuntime>,
    timeout_ns: Option<u64>,
    hold: Option<OperationHold>,
) -> Result<Option<Arc<Reply>>, IpcError> {
    let deadline_ns = deadline_after(runtime, timeout_ns);
    match port.recv(receiver, runtime, hold) {
        RecvAction::Done { reply } => Ok(reply),
        RecvAction::Failed(e) => Err(e),
        RecvAction::SenderGone { badge } => Err(publish_departed_badge(receiver, badge)),
        RecvAction::Park {
            waker,
            outcome,
            reply_slot,
        } => {
            arm_port_cancel(port, runtime, &waker, &outcome, Some(reply_slot.clone()));
            let resolved = resolve_send_recv(port, runtime, &waker, &outcome, deadline_ns);
            runtime.clear_blocked_cancel();
            match outcome.try_get() {
                Ok(Some(RendezvousOutcome::SenderGone(badge))) => {
                    return Err(publish_departed_badge(receiver, badge));
                }
                Err(_) => return Err(IpcError::Canceled),
                _ => {}
            }
            resolved?;

            Ok(reply_slot.take())
        }
    }
}

fn publish_departed_badge(receiver: &ThreadTransport, badge: u64) -> IpcError {
    match write_badge(receiver, badge) {
        Ok(()) => IpcError::SenderGone,
        Err(e) => e,
    }
}

impl Drop for Port {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

struct PortCancel {
    port: Arc<Port>,
    waker: Arc<ParkWaker>,
    outcome: Arc<OutcomeSlot>,
    reply_slot: Option<Arc<ReplySlot>>,
}

impl CancelTarget for PortCancel {
    fn cancel(&self) {
        if !self.port.remove_waiter(&self.waker) {
            self.outcome.cancel_after_dequeue();
        }
    }

    fn take_abandoned_reply(&self) -> Option<Arc<Reply>> {
        self.reply_slot.as_ref().and_then(|slot| slot.take())
    }
}

fn arm_port_cancel(
    port: &Arc<Port>,
    runtime: &Arc<dyn KernelRuntime>,
    waker: &Arc<ParkWaker>,
    outcome: &Arc<OutcomeSlot>,
    reply_slot: Option<Arc<ReplySlot>>,
) {
    let cancel: Arc<dyn CancelTarget> = Arc::new(PortCancel {
        port: port.clone(),
        waker: waker.clone(),
        outcome: outcome.clone(),
        reply_slot,
    });
    runtime.set_blocked_cancel(cancel);
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::{
        num::NonZeroU64,
        sync::atomic::{AtomicBool, AtomicU32, Ordering},
    };

    use collections::MutexCell;
    use memory::virtual_address::VirtualAddress;
    use syscall::encode_tag;

    use super::{
        super::{
            CancelTarget, DrainedHandles, HandleTable,
            handle::Capability,
            ipc_buffer_xfer::test_mapper::PageMapper,
            rev_node::revoke_subtree,
            runtime::{KernelRuntime, WaitToken},
            target::CapabilityTarget,
        },
        *,
    };

    const BASE_A: usize = 0x4000_0000;
    const BASE_B: usize = 0x5000_0000;
    const BADGE_A: u64 = 0xA1;
    const BADGE_B: u64 = 0xB2;

    struct StubRuntime {
        unblocks: AtomicU32,
    }

    impl StubRuntime {
        fn arc() -> Arc<dyn KernelRuntime> {
            Arc::new(Self {
                unblocks: AtomicU32::new(0),
            })
        }
    }

    impl KernelRuntime for StubRuntime {
        fn current_wait_token(&self) -> WaitToken {
            WaitToken::new(NonZeroU64::new(1).unwrap())
        }
        fn now_ns(&self) -> u64 {
            0
        }
        fn current_handle_table(&self) -> Option<Arc<MutexCell<HandleTable>>> {
            None
        }
        fn exit_current_thread(&self, _c: i32) -> ! {
            unreachable!()
        }
        fn exit_current_process(&self, _c: i32) -> ! {
            unreachable!("not used in port tests")
        }
        fn yield_current(&self) {}
        fn block_current_until(&self, _f: &AtomicU32, _t: Option<u64>) {}
        fn unblock(&self, _t: WaitToken) {
            self.unblocks.fetch_add(1, Ordering::AcqRel);
        }
        fn set_blocked_cancel(&self, _cancel: Arc<dyn CancelTarget>) {}
        fn clear_blocked_cancel(&self) {}
    }

    struct DeadlineRuntime {
        now: core::sync::atomic::AtomicU64,
        parks: std::sync::Mutex<Vec<Option<u64>>>,
        on_first_park: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
    }

    impl DeadlineRuntime {
        fn arc(now: u64) -> Arc<Self> {
            Arc::new(Self {
                now: core::sync::atomic::AtomicU64::new(now),
                parks: std::sync::Mutex::new(Vec::new()),
                on_first_park: std::sync::Mutex::new(None),
            })
        }

        fn set_first_park_hook(&self, hook: Box<dyn FnOnce() + Send>) {
            *self.on_first_park.lock().unwrap() = Some(hook);
        }

        fn parks(&self) -> Vec<Option<u64>> {
            self.parks.lock().unwrap().clone()
        }
    }

    impl KernelRuntime for DeadlineRuntime {
        fn current_wait_token(&self) -> WaitToken {
            WaitToken::new(NonZeroU64::new(1).unwrap())
        }
        fn now_ns(&self) -> u64 {
            self.now.load(Ordering::Acquire)
        }
        fn current_handle_table(&self) -> Option<Arc<MutexCell<HandleTable>>> {
            None
        }
        fn exit_current_thread(&self, _c: i32) -> ! {
            unreachable!()
        }
        fn exit_current_process(&self, _c: i32) -> ! {
            unreachable!("not used in port tests")
        }
        fn yield_current(&self) {}
        fn block_current_until(&self, _f: &AtomicU32, timeout_ns: Option<u64>) {
            self.parks.lock().unwrap().push(timeout_ns);
            if let Some(timeout) = timeout_ns {
                self.now.fetch_add(timeout, Ordering::AcqRel);
            }
            if let Some(hook) = self.on_first_park.lock().unwrap().take() {
                hook();
            }
        }
        fn unblock(&self, _t: WaitToken) {}
        fn set_blocked_cancel(&self, _cancel: Arc<dyn CancelTarget>) {}
        fn clear_blocked_cancel(&self) {}
    }

    fn transport(base: usize) -> ThreadTransport {
        use memory::memory_mapper::MemoryMapper;

        let mapper = PageMapper::new(base);
        let table = Arc::new(MutexCell::new(HandleTable::new()));

        mapper
            .copy_user_out(VirtualAddress::new(base), &encode_tag(3, 0).to_le_bytes())
            .unwrap();
        mapper
            .copy_user_out(VirtualAddress::new(base + 24), b"abc")
            .unwrap();
        ThreadTransport::new(mapper, VirtualAddress::new(base), table)
    }

    fn receiver_capability(port: &Arc<Port>) -> Capability {
        Capability::new(CapabilityTarget::Port(port.clone()), Rights::READ)
    }

    fn root_capability(port: &Arc<Port>) -> Capability {
        Capability::new(
            CapabilityTarget::Port(port.clone()),
            Rights::READ | Rights::WRITE | Rights::DUPLICATE,
        )
    }

    fn badged_capability(root: &Capability, badge: u64) -> Capability {
        root.duplicate(Rights::WRITE, badge)
            .expect("mint badged capability")
    }

    #[test]
    fn recv_with_no_sender_parks_as_receiver() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let action = ep.recv(&transport(BASE_B), &rt, None);
        assert!(matches!(action, RecvAction::Park { .. }));
        assert!(!ep.queue_is_empty());
    }

    #[test]
    fn send_matches_parked_receiver_and_delivers() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let _receiver = receiver_capability(&ep);

        // Получатель паркуется.
        let recv_action = ep.recv(&transport(BASE_B), &rt, None);
        let RecvAction::Park {
            outcome: recv_outcome,
            reply_slot: recv_reply_slot,
            ..
        } = recv_action
        else {
            panic!("recv must park");
        };
        assert!(recv_outcome.get().is_none());

        // Отправитель встречает получателя: матч, доставка, queue пуст.
        let send_action = ep.send(&transport(BASE_A), &rt, None);
        assert!(matches!(send_action, SendAction::Done));
        assert_eq!(recv_outcome.get(), Some(RendezvousOutcome::Delivered));
        assert!(ep.queue_is_empty());
        // send -> recv: reply отсутствует.
        assert!(recv_reply_slot.take().is_none());
    }

    #[test]
    fn send_with_no_receiver_parks_as_sender() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let _receiver = receiver_capability(&ep);
        let action = ep.send(&transport(BASE_A), &rt, None);
        assert!(matches!(action, SendAction::Park { .. }));
        assert!(!ep.queue_is_empty());
    }

    #[test]
    fn recv_matches_parked_sender() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let _receiver = receiver_capability(&ep);
        let send_action = ep.send(&transport(BASE_A), &rt, None);
        let SendAction::Park {
            outcome: send_outcome,
            ..
        } = send_action
        else {
            panic!("send must park");
        };

        let recv_action = ep.recv(&transport(BASE_B), &rt, None);
        assert!(matches!(recv_action, RecvAction::Done { reply: None }));
        assert_eq!(send_outcome.get(), Some(RendezvousOutcome::Delivered));
        assert!(ep.queue_is_empty());
    }

    #[test]
    fn call_matched_by_recv_yields_reply() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let _receiver = receiver_capability(&ep);
        // вызывающая сторона паркуется (call всегда паркуется).
        let ctx = ParkCtx::for_current(&rt);
        let outcome = ctx.outcome.clone();
        let call_action = ep.call(&transport(BASE_A), ctx, None);
        assert!(matches!(call_action, CallAction::Park { .. }));

        // Получатель смэтчил call -> получает Reply, вызывающая сторона остаётся
        // заблокированным (outcome ещё pending).
        let recv_action = ep.recv(&transport(BASE_B), &rt, None);
        let RecvAction::Done { reply: Some(reply) } = recv_action else {
            panic!("recv must return a reply for call");
        };
        assert!(outcome.get().is_none(), "caller stays blocked until reply");

        // reply доставляет ответ вызывателю -> outcome Delivered.
        let server = transport(BASE_B);
        reply.reply(&server).expect("reply ok");
        assert_eq!(outcome.get(), Some(RendezvousOutcome::Delivered));
        // one-shot: повторный reply отвергнут.
        assert_eq!(reply.reply(&server).unwrap_err(), IpcError::BadHandle);
    }

    #[test]
    fn cancel_all_wakes_waiters_peer_gone() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let action = ep.recv(&transport(BASE_B), &rt, None);
        let RecvAction::Park { outcome, .. } = action else {
            panic!("recv must park");
        };
        ep.cancel_all();
        assert_eq!(outcome.get(), Some(RendezvousOutcome::PeerGone));
        assert!(ep.queue_is_empty());
    }

    #[test]
    fn drop_port_cancels_parked_waiter() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let action = ep.recv(&transport(BASE_B), &rt, None);
        let RecvAction::Park { outcome, .. } = action else {
            panic!("recv must park");
        };
        drop(ep);
        assert_eq!(outcome.get(), Some(RendezvousOutcome::PeerGone));
    }

    #[test]
    fn port_cancel_removes_waiter_on_termination() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let RecvAction::Park { waker, outcome, .. } = ep.recv(&transport(BASE_B), &rt, None) else {
            panic!("recv must park");
        };
        assert!(!ep.queue_is_empty());

        // Терминация заблокированного потока: scheduler зовёт PortCancel,
        // снимающий Waiter с очереди - матчер позже не доставит в мёртвый поток.
        let cancel = PortCancel {
            port: ep.clone(),
            waker: waker.clone(),
            outcome: outcome.clone(),
            reply_slot: None,
        };
        CancelTarget::cancel(&cancel);
        assert!(ep.queue_is_empty());

        // Идемпотентность: повторный cancel - no-op.
        CancelTarget::cancel(&cancel);
        assert!(ep.queue_is_empty());
    }

    #[test]
    fn port_cancel_waits_for_dequeued_transfer() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let _receiver = receiver_capability(&ep);
        let RecvAction::Park { waker, outcome, .. } = ep.recv(&transport(BASE_B), &rt, None) else {
            panic!("recv must park");
        };
        let receiver = ep
            .inner
            .with_lock(PortState::take_receiver)
            .expect("receiver set remains live")
            .expect("parked receiver is dequeued");
        assert_eq!(outcome.raw(), OutcomeSlot::DELIVERING);

        let cancel = PortCancel {
            port: ep,
            waker,
            outcome: outcome.clone(),
            reply_slot: None,
        };
        let completed = Arc::new(AtomicBool::new(false));
        let completed_by_cancel = completed.clone();
        let cancel_thread = std::thread::spawn(move || {
            CancelTarget::cancel(&cancel);
            completed_by_cancel.store(true, Ordering::Release);
        });

        while outcome.cancel_waiter_count() == 0 {
            std::thread::yield_now();
        }
        assert!(!completed.load(Ordering::Acquire));

        receiver.outcome.set(RendezvousOutcome::Delivered);
        receiver.waker.signal_match();
        cancel_thread.join().expect("cancel joins");
        assert!(completed.load(Ordering::Acquire));
    }

    #[test]
    fn port_cancel_phase2_timeouts_caller_and_rejects_late_reply() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let _receiver = receiver_capability(&ep);
        // вызывающая сторона паркуется (call всегда паркуется до reply).
        let ctx = ParkCtx::for_current(&rt);
        let (waker, outcome) = (ctx.waker.clone(), ctx.outcome.clone());
        let call_action = ep.call(&transport(BASE_A), ctx, None);
        assert!(matches!(call_action, CallAction::Park { .. }));

        // Получатель забрал запрос -> вызывающая сторона в фазе 2 (AWAITING_REPLY); его
        // Waiter уже не в очереди, поэтому remove_waiter в cancel - no-op.
        let RecvAction::Done { reply: Some(reply) } = ep.recv(&transport(BASE_B), &rt, None) else {
            panic!("recv must return a reply for call");
        };
        assert_eq!(outcome.raw(), OutcomeSlot::AWAITING_REPLY);

        // Терминация: PortCancel переводит слот в TIMEDOUT (фаза 2) - так
        // поздний reply сервера не пишет в освобождаемый IPC-буфер.
        let cancel = PortCancel {
            port: ep.clone(),
            waker: waker.clone(),
            outcome: outcome.clone(),
            reply_slot: None,
        };
        CancelTarget::cancel(&cancel);
        assert_eq!(outcome.raw(), OutcomeSlot::TIMEDOUT);

        // Поздний reply отвергнут - сервер получает PeerClosed.
        let server = transport(BASE_B);
        assert_eq!(reply.reply(&server).unwrap_err(), IpcError::PeerClosed);

        // Идемпотентность: повторный cancel - no-op.
        CancelTarget::cancel(&cancel);
        assert_eq!(outcome.raw(), OutcomeSlot::TIMEDOUT);
    }

    #[test]
    fn port_cancel_releases_reply_abandoned_by_receiver() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let _receiver = receiver_capability(&ep);
        let RecvAction::Park {
            waker: receiver_waker,
            outcome: receiver_outcome,
            reply_slot,
        } = ep.recv(&transport(BASE_B), &rt, None)
        else {
            panic!("recv must park with a reply slot");
        };

        let caller_ctx = ParkCtx::for_current(&rt);
        let (caller_waker, caller_outcome) = (caller_ctx.waker.clone(), caller_ctx.outcome.clone());
        let call_action = ep.call(&transport(BASE_A), caller_ctx, None);
        assert!(matches!(call_action, CallAction::Park { .. }));
        assert_eq!(caller_outcome.raw(), OutcomeSlot::AWAITING_REPLY);

        // Терминация получателя между матчем и изъятием reply: Waiter уже снят
        // с очереди, и reply достижим только через слот.
        let cancel = PortCancel {
            port: ep.clone(),
            waker: receiver_waker,
            outcome: receiver_outcome,
            reply_slot: Some(reply_slot.clone()),
        };
        CancelTarget::cancel(&cancel);
        let abandoned = cancel
            .take_abandoned_reply()
            .expect("cancel yields the abandoned reply");
        // Слот опустошён: нормальный путь изъятия того же reply не получит.
        assert!(reply_slot.take().is_none());
        assert!(cancel.take_abandoned_reply().is_none());

        abandoned.cancel();
        assert_eq!(caller_outcome.get(), Some(RendezvousOutcome::PeerGone));
        assert_eq!(
            caller_waker.state().load(Ordering::Acquire),
            ParkState::CANCELED
        );
    }

    #[test]
    fn remove_waiter_is_arbiter_and_idempotent() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let RecvAction::Park { waker, .. } = ep.recv(&transport(BASE_B), &rt, None) else {
            panic!("recv must park");
        };
        assert!(!ep.queue_is_empty());
        // Снятие найденного waiter -> true, очередь пустеет.
        assert!(ep.remove_waiter(&waker));
        assert!(ep.queue_is_empty());
        // Идемпотентность: повторное снятие -> false.
        assert!(!ep.remove_waiter(&waker));
    }

    #[test]
    fn port_send_poll_without_receiver_is_timeout() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let _receiver = receiver_capability(&ep);
        // poll (Some(0)) без получателя не блокируется - сразу Timeout, и
        // отправитель снят с очереди.
        assert_eq!(
            port_send(&ep, &transport(BASE_A), &rt, Some(0), None),
            Err(IpcError::Timeout)
        );
        assert!(ep.queue_is_empty());
    }

    #[test]
    fn port_recv_poll_without_sender_is_timeout() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        assert_eq!(
            port_recv(&ep, &transport(BASE_B), &rt, Some(0), None).map(|_| ()),
            Err(IpcError::Timeout)
        );
        assert!(ep.queue_is_empty());
    }

    #[test]
    fn port_send_poll_matches_parked_receiver() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let _receiver = receiver_capability(&ep);
        // Получатель уже припаркован - poll-send встречает его немедленно.
        let RecvAction::Park {
            outcome: recv_outcome,
            ..
        } = ep.recv(&transport(BASE_B), &rt, None)
        else {
            panic!("recv must park");
        };
        assert_eq!(
            port_send(&ep, &transport(BASE_A), &rt, Some(0), None),
            Ok(())
        );
        assert_eq!(recv_outcome.get(), Some(RendezvousOutcome::Delivered));
        assert!(ep.queue_is_empty());
    }

    #[test]
    fn outcome_slot_reply_timeout_arbitration() {
        let a = OutcomeSlot::new();
        a.set_awaiting_reply();
        assert!(a.try_timeout());
        assert!(!a.try_begin_reply()); // reply отклонён - вызывающая сторона ушёл

        let b = OutcomeSlot::new();
        b.set_awaiting_reply();
        assert!(b.try_begin_reply());
        assert!(!b.try_timeout()); // тайм-аут проиграл - reply коммитнут
    }

    #[test]
    fn resolve_call_woken_early_sleeps_the_remainder_before_timing_out() {
        let rt = DeadlineRuntime::arc(500);
        let dyn_rt: Arc<dyn KernelRuntime> = rt.clone();
        let ep = Port::new();
        let ctx = ParkCtx::for_current(&dyn_rt);
        ctx.outcome.set_awaiting_reply();

        let result = resolve_call(&ep, &dyn_rt, &ctx.waker, &ctx.outcome, Some(1000));

        assert_eq!(result, Err(IpcError::Timeout));
        // Единственная перепарковка - ровно на остаток до дедлайна (1000 - 500).
        assert_eq!(rt.parks(), alloc::vec![Some(500)]);
        assert_eq!(ctx.outcome.raw(), OutcomeSlot::TIMEDOUT);
    }

    /// Тихий переход в AWAITING_REPLY вызывателя не будит и переобнаруживается
    /// коротким шагом перепарковки.
    #[test]
    fn resolve_call_expired_deadline_waits_out_an_in_flight_transfer() {
        let rt = DeadlineRuntime::arc(2000);
        let dyn_rt: Arc<dyn KernelRuntime> = rt.clone();
        let ep = Port::new();
        let ctx = ParkCtx::for_current(&dyn_rt);
        let outcome = ctx.outcome.clone();
        rt.set_first_park_hook(Box::new(move || outcome.set_awaiting_reply()));

        let result = resolve_call(&ep, &dyn_rt, &ctx.waker, &ctx.outcome, Some(1000));

        assert_eq!(result, Err(IpcError::Timeout));
        assert_eq!(rt.parks(), alloc::vec![Some(EXPIRED_RECHECK_NS)]);
        assert_eq!(ctx.outcome.raw(), OutcomeSlot::TIMEDOUT);
    }

    /// Бессрочный приём тайм-аута не знает: пробуждение, заставшее waiter в очереди,
    /// возвращает поток в ожидание.
    #[test]
    fn resolve_recv_without_deadline_waits_out_a_wake_without_resolution() {
        let rt = DeadlineRuntime::arc(0);
        let dyn_rt: Arc<dyn KernelRuntime> = rt.clone();
        let ep = Port::new();
        let _receiver = receiver_capability(&ep);
        let RecvAction::Park { waker, outcome, .. } = ep.recv(&transport(BASE_B), &dyn_rt, None)
        else {
            panic!("recv must park");
        };

        // Разрешение приходит встречным отправителем на следующей парковке.
        let (matcher, matcher_rt) = (ep.clone(), dyn_rt.clone());
        rt.set_first_park_hook(Box::new(move || {
            assert!(matches!(
                matcher.send(&transport(BASE_A), &matcher_rt, None),
                SendAction::Done
            ));
        }));

        let result = resolve_send_recv(&ep, &dyn_rt, &waker, &outcome, None);

        assert_eq!(result, Ok(()));
        assert_eq!(rt.parks(), alloc::vec![None]);
    }

    /// Отправитель на бессрочной операции ведёт себя так же.
    #[test]
    fn resolve_send_without_deadline_waits_out_a_wake_without_resolution() {
        let rt = DeadlineRuntime::arc(0);
        let dyn_rt: Arc<dyn KernelRuntime> = rt.clone();
        let ep = Port::new();
        let _receiver = receiver_capability(&ep);
        let SendAction::Park { waker, outcome } = ep.send(&transport(BASE_A), &dyn_rt, None) else {
            panic!("send must park");
        };

        let (matcher, matcher_rt) = (ep.clone(), dyn_rt.clone());
        rt.set_first_park_hook(Box::new(move || {
            assert!(matches!(
                matcher.recv(&transport(BASE_B), &matcher_rt, None),
                RecvAction::Done { reply: None }
            ));
        }));

        let result = resolve_send_recv(&ep, &dyn_rt, &waker, &outcome, None);

        assert_eq!(result, Ok(()));
        assert_eq!(rt.parks(), alloc::vec![None]);
    }

    /// Бессрочный `call`, разбуженный в очереди отправителей, возвращается в ожидание.
    #[test]
    fn resolve_call_without_deadline_waits_out_a_wake_without_resolution() {
        let rt = DeadlineRuntime::arc(0);
        let dyn_rt: Arc<dyn KernelRuntime> = rt.clone();
        let ep = Port::new();
        let _receiver = receiver_capability(&ep);
        let ctx = ParkCtx::for_current(&dyn_rt);
        let (waker, outcome) = (ctx.waker.clone(), ctx.outcome.clone());
        assert!(matches!(
            ep.call(&transport(BASE_A), ctx, None),
            CallAction::Park { .. }
        ));

        // Разрешение приходит следующей парковкой: сервер забирает запрос и отвечает.
        let (server, server_rt) = (ep.clone(), dyn_rt.clone());
        rt.set_first_park_hook(Box::new(move || {
            let RecvAction::Done { reply: Some(reply) } =
                server.recv(&transport(BASE_B), &server_rt, None)
            else {
                panic!("recv must return a reply for call");
            };
            reply.reply(&transport(BASE_B)).expect("reply ok");
        }));

        let result = resolve_call(&ep, &dyn_rt, &waker, &outcome, None);

        assert_eq!(result, Ok(()));
        assert_eq!(rt.parks(), alloc::vec![None]);
    }

    /// Приём со сроком: пробуждение раньше дедлайна досыпает остаток.
    #[test]
    fn resolve_recv_woken_early_sleeps_the_remainder_before_timing_out() {
        let rt = DeadlineRuntime::arc(500);
        let dyn_rt: Arc<dyn KernelRuntime> = rt.clone();
        let ep = Port::new();
        let RecvAction::Park { waker, outcome, .. } = ep.recv(&transport(BASE_B), &dyn_rt, None)
        else {
            panic!("recv must park");
        };

        let result = resolve_send_recv(&ep, &dyn_rt, &waker, &outcome, Some(1000));

        assert_eq!(result, Err(IpcError::Timeout));
        // Единственная перепарковка - ровно на остаток до дедлайна (1000 - 500).
        assert_eq!(rt.parks(), alloc::vec![Some(500)]);
        assert!(ep.queue_is_empty());
    }

    #[test]
    fn call_caller_timeout_before_reply_rejects_server_reply() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let _receiver = receiver_capability(&ep);
        // вызывающая сторона паркуется (call всегда паркуется до reply).
        let ctx = ParkCtx::for_current(&rt);
        let (waker, outcome) = (ctx.waker.clone(), ctx.outcome.clone());
        let call_action = ep.call(&transport(BASE_A), ctx, None);
        assert!(matches!(call_action, CallAction::Park { .. }));

        let RecvAction::Done { reply: Some(reply) } = ep.recv(&transport(BASE_B), &rt, None) else {
            panic!("recv must return a reply for call");
        };
        assert_eq!(outcome.raw(), OutcomeSlot::AWAITING_REPLY);

        assert!(outcome.try_timeout());
        assert!(waker.claim_timeout());

        let server = transport(BASE_B);
        assert_eq!(reply.reply(&server).unwrap_err(), IpcError::PeerClosed);
        assert_eq!(outcome.raw(), OutcomeSlot::TIMEDOUT);
    }

    #[test]
    fn send_and_call_without_receivers_are_refused_before_parking() {
        let rt = StubRuntime::arc();
        let ep = Port::new();

        let sent = ep.send(&transport(BASE_A), &rt, None);
        assert!(matches!(sent, SendAction::Failed(IpcError::PeerClosed)));
        assert!(ep.queue_is_empty());

        let called = ep.call(&transport(BASE_A), ParkCtx::for_current(&rt), None);
        assert!(matches!(called, CallAction::Failed(IpcError::PeerClosed)));
        assert!(ep.queue_is_empty());
    }

    #[test]
    fn last_receiver_capability_wakes_parked_sender() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let receiver = receiver_capability(&ep);
        let SendAction::Park { outcome, .. } = ep.send(&transport(BASE_A), &rt, None) else {
            panic!("send must park");
        };

        drop(receiver);

        assert_eq!(outcome.get(), Some(RendezvousOutcome::PeerGone));
        assert!(ep.queue_is_empty());
    }

    #[test]
    fn last_receiver_capability_wakes_parked_receiver() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let receiver = receiver_capability(&ep);
        let RecvAction::Park { outcome, .. } = ep.recv(&transport(BASE_B), &rt, None) else {
            panic!("recv must park");
        };

        // Бессрочная парковка дедлайна не имеет: получателя обязано разбудить
        // само обнуление множества.
        drop(receiver);

        assert_eq!(outcome.get(), Some(RendezvousOutcome::PeerGone));
        assert!(ep.queue_is_empty());
    }

    #[test]
    fn departed_badge_waits_for_the_next_receiver() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let root = root_capability(&ep);
        let client = badged_capability(&root, BADGE_A);

        // Получателя в момент ухода нет - событие ждёт очередного recv.
        drop(client);

        let picked = ep.recv(&transport(BASE_B), &rt, None);
        assert!(matches!(picked, RecvAction::SenderGone { badge } if badge == BADGE_A));
        // Событие одноразовое.
        assert!(matches!(
            ep.recv(&transport(BASE_B), &rt, None),
            RecvAction::Park { .. }
        ));
    }

    #[test]
    fn departed_badge_reaches_exactly_one_of_two_receivers() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let root = root_capability(&ep);
        let client = badged_capability(&root, BADGE_A);
        let RecvAction::Park { outcome: first, .. } = ep.recv(&transport(BASE_B), &rt, None) else {
            panic!("first recv must park");
        };
        let RecvAction::Park {
            outcome: second, ..
        } = ep.recv(&transport(BASE_B), &rt, None)
        else {
            panic!("second recv must park");
        };

        drop(client);

        let outcomes = [first.get(), second.get()];
        assert_eq!(
            outcomes
                .iter()
                .filter(|o| matches!(o, Some(RendezvousOutcome::SenderGone(BADGE_A))))
                .count(),
            1
        );
        // Второй остался припаркованным - событие не размножается.
        assert!(outcomes.contains(&None));
        assert!(!ep.queue_is_empty());
    }

    #[test]
    fn departed_badge_precedes_a_parked_sender() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let root = root_capability(&ep);
        let live = badged_capability(&root, BADGE_A);
        let leaving = badged_capability(&root, BADGE_B);
        let SendAction::Park {
            outcome: sender, ..
        } = ep.send(&transport(BASE_A).with_badge(BADGE_A), &rt, None)
        else {
            panic!("send must park");
        };

        drop(leaving);

        // Событие идёт впереди очереди отправителей, начатый кадр не тронут.
        let picked = ep.recv(&transport(BASE_B), &rt, None);
        assert!(matches!(picked, RecvAction::SenderGone { badge } if badge == BADGE_B));
        assert!(sender.get().is_none());

        // Кадр забирает следующий recv.
        assert!(matches!(
            ep.recv(&transport(BASE_B), &rt, None),
            RecvAction::Done { reply: None }
        ));
        assert_eq!(sender.get(), Some(RendezvousOutcome::Delivered));
        drop(live);
    }

    #[test]
    fn departed_badge_removes_its_parked_senders() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let root = root_capability(&ep);
        let client = badged_capability(&root, BADGE_A);
        let SendAction::Park {
            outcome: sender, ..
        } = ep.send(&transport(BASE_A).with_badge(BADGE_A), &rt, None)
        else {
            panic!("send must park");
        };

        // Кадр снесённой сессии серверу не достаётся.
        drop(client);

        assert_eq!(sender.get(), Some(RendezvousOutcome::PeerGone));
        assert!(ep.queue_is_empty());
        let picked = ep.recv(&transport(BASE_B), &rt, None);
        assert!(matches!(picked, RecvAction::SenderGone { badge } if badge == BADGE_A));
    }

    #[test]
    fn minting_a_badge_again_retracts_its_unread_event() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let root = root_capability(&ep);
        let client = badged_capability(&root, BADGE_A);
        drop(client);

        let _revived = badged_capability(&root, BADGE_A);

        assert!(matches!(
            ep.recv(&transport(BASE_B), &rt, None),
            RecvAction::Park { .. }
        ));
    }

    #[test]
    fn badge_recycling_keeps_the_departure_ledger_consistent() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let root = root_capability(&ep);

        // Уход без получателя, затем минт того же значения и второй уход - уже
        // при припаркованном получателе.
        drop(badged_capability(&root, BADGE_A));
        let revived = badged_capability(&root, BADGE_A);
        let RecvAction::Park { outcome, .. } = ep.recv(&transport(BASE_B), &rt, None) else {
            panic!("recv must park");
        };
        drop(revived);
        assert_eq!(outcome.get(), Some(RendezvousOutcome::SenderGone(BADGE_A)));

        // Непрочтённых событий не осталось - следующий recv паркуется.
        assert!(matches!(
            ep.recv(&transport(BASE_B), &rt, None),
            RecvAction::Park { .. }
        ));
        // Значение остаётся переиспользуемым.
        drop(badged_capability(&root, BADGE_A));
    }

    fn accumulate_departed_badges(root: &Capability, count: u64) {
        for badge in 1..=count {
            drop(badged_capability(root, badge));
        }
    }

    const EXTRA_BADGE: u64 = DEPARTED_BADGE_LIMIT as u64 + 1;

    #[test]
    fn a_new_badge_is_refused_when_departed_events_are_unread() {
        let ep = Port::new();
        let root = root_capability(&ep);
        accumulate_departed_badges(&root, DEPARTED_BADGE_LIMIT as u64);

        assert_eq!(
            root.duplicate(Rights::WRITE, EXTRA_BADGE).err(),
            Some(IpcError::ResourceExhausted)
        );
        // Значение с уже заведённой записью оживает и на потолке.
        let _revived = badged_capability(&root, 1);
    }

    #[test]
    fn reading_a_departed_badge_frees_its_record() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let root = root_capability(&ep);
        accumulate_departed_badges(&root, DEPARTED_BADGE_LIMIT as u64);

        assert!(matches!(
            ep.recv(&transport(BASE_B), &rt, None),
            RecvAction::SenderGone { .. }
        ));
        let _fresh = badged_capability(&root, EXTRA_BADGE);
    }

    #[test]
    fn the_limit_counts_departed_badges_and_not_live_ones() {
        let ep = Port::new();
        let root = root_capability(&ep);

        // Записи с держателями потолком не ограничены.
        let live: Vec<Capability> = (1..=EXTRA_BADGE)
            .map(|badge| badged_capability(&root, badge))
            .collect();

        // Те же записи, оставшись без держателей, упирают порт в потолок.
        drop(live);
        assert_eq!(
            root.duplicate(Rights::WRITE, EXTRA_BADGE + 1).err(),
            Some(IpcError::ResourceExhausted)
        );
    }

    #[test]
    fn two_capabilities_of_one_badge_form_one_set() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let root = root_capability(&ep);
        let first = badged_capability(&root, BADGE_A);
        let second = badged_capability(&root, BADGE_A);

        drop(first);
        let RecvAction::Park { outcome, .. } = ep.recv(&transport(BASE_B), &rt, None) else {
            panic!("half-empty badge set raises no event");
        };

        // Уход объявляется, когда исчезла вторая capability того же значка.
        drop(second);
        assert_eq!(outcome.get(), Some(RendezvousOutcome::SenderGone(BADGE_A)));
        assert!(ep.queue_is_empty());
    }

    #[test]
    fn transfer_between_tables_keeps_holder_sets() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let mut source = HandleTable::new();
        let mut destination = HandleTable::new();
        let id = source
            .insert(Capability::new(
                CapabilityTarget::Port(ep.clone()),
                Rights::READ | Rights::TRANSFER,
            ))
            .expect("insert receiver capability");

        let mut drained = DrainedHandles::empty();
        source
            .try_drain_for_transfer(&[id], Rights::TRANSFER, &mut drained)
            .expect("transfer the receiver capability");
        let moved = drained.take_handles();
        drained.release();
        destination
            .insert(moved.into_iter().next().expect("single capability"))
            .expect("insert into destination");

        // Перенос переместил значение - множество получателей не изменилось.
        assert!(matches!(
            ep.send(&transport(BASE_A), &rt, None),
            SendAction::Park { .. }
        ));

        drop(destination);
        assert!(matches!(
            ep.send(&transport(BASE_A), &rt, None),
            SendAction::Failed(IpcError::PeerClosed)
        ));
    }

    #[test]
    fn revoked_capability_stops_counting_as_receiver() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let mut grantor = HandleTable::new();
        let mut recipient = HandleTable::new();
        let root = grantor
            .insert(Capability::new(
                CapabilityTarget::Port(ep.clone()),
                Rights::READ | Rights::DUPLICATE | Rights::TRANSFER,
            ))
            .expect("insert receiver capability");
        let derived = grantor
            .duplicate(root, Rights::READ | Rights::TRANSFER, 0)
            .expect("derive receiver capability");
        let mut drained = DrainedHandles::empty();
        grantor
            .try_drain_for_transfer(&[derived], Rights::TRANSFER, &mut drained)
            .expect("hand the derived capability over");
        let moved = drained.take_handles();
        drained.release();
        recipient
            .insert(moved.into_iter().next().expect("single capability"))
            .expect("insert into recipient");

        // Учёт снимает отзыв: производная капа жива у получателя, но её
        // держатель уже снят каскадом от закрытого корня.
        grantor
            .remove(root)
            .expect("remove the root capability")
            .release();
        assert!(matches!(
            ep.send(&transport(BASE_A), &rt, None),
            SendAction::Failed(IpcError::PeerClosed)
        ));

        // Дроп после отзыва учёта второй раз не снимает.
        drop(recipient);
        assert!(matches!(
            ep.send(&transport(BASE_A), &rt, None),
            SendAction::Failed(IpcError::PeerClosed)
        ));
    }

    #[test]
    fn a_port_lookup_hands_the_operation_its_hold() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let mut table = HandleTable::new();
        let id = table
            .insert(Capability::new(
                CapabilityTarget::Port(ep.clone()),
                Rights::READ,
            ))
            .expect("insert receiver capability");

        let access = table.get_port(id, Rights::READ).expect("port is readable");
        assert!(access.hold.is_some());

        // Пока удержание живо, закрытие последней capability множества получателей не обнуляет.
        table
            .remove(id)
            .expect("remove the receiver capability")
            .release();
        assert!(matches!(
            ep.send(&transport(BASE_A), &rt, None),
            SendAction::Park { .. }
        ));

        drop(access);
        assert!(matches!(
            ep.send(&transport(BASE_A), &rt, None),
            SendAction::Failed(IpcError::PeerClosed)
        ));
    }

    /// Regression guard на гонку revoke между регистрацией хука и активацией.
    #[test]
    fn releasing_an_unactivated_ticket_neither_uncounts_nor_wakes() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let _receiver = receiver_capability(&ep);
        let SendAction::Park {
            outcome: sender, ..
        } = ep.send(&transport(BASE_A), &rt, None)
        else {
            panic!("send must park");
        };

        let ticket = ep
            .holder_ticket(Rights::READ)
            .expect("port hands out receiver tickets");
        ticket.release();

        // Множество получателей не обнулялось - отправитель остаётся в очереди.
        assert!(sender.get().is_none());
        assert!(!ep.queue_is_empty());

        assert_eq!(ticket.activate().unwrap_err(), IpcError::Revoked);
        assert!(sender.get().is_none());
    }

    #[test]
    fn an_unactivated_ticket_leaves_the_receiver_accounting_intact() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let receiver = receiver_capability(&ep);
        let ticket = ep
            .holder_ticket(Rights::READ)
            .expect("port hands out receiver tickets");
        ticket.release();

        let SendAction::Park {
            outcome: sender, ..
        } = ep.send(&transport(BASE_A), &rt, None)
        else {
            panic!("send must park");
        };

        drop(receiver);
        assert_eq!(sender.get(), Some(RendezvousOutcome::PeerGone));
        assert!(ep.queue_is_empty());
    }

    #[test]
    fn a_revoked_capability_hands_out_no_hold() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let receiver = receiver_capability(&ep);

        // Отзыв снял учёт этой capability, и удержание его не оживляет.
        revoke_subtree(receiver.node());

        assert!(receiver.hold_for_operation().is_none());
        assert!(matches!(
            ep.send(&transport(BASE_A), &rt, None),
            SendAction::Failed(IpcError::PeerClosed)
        ));
    }

    #[test]
    fn an_in_flight_recv_holds_its_role_until_it_is_queued() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let receiver = receiver_capability(&ep);

        // Последняя capability получателя закрывается, когда recv уже начат и ещё не в очереди.
        let hold = receiver.hold_for_operation();
        drop(receiver);

        let RecvAction::Park { outcome, .. } = ep.recv(&transport(BASE_B), &rt, hold) else {
            panic!("recv must park");
        };

        // Обнуление множества застало waiter в очереди, а не пустой порт.
        assert_eq!(outcome.get(), Some(RendezvousOutcome::PeerGone));
        assert!(ep.queue_is_empty());
    }

    #[test]
    fn an_in_flight_send_delivers_its_frame_before_its_badge_departs() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let root = root_capability(&ep);
        let client = badged_capability(&root, BADGE_A);
        let RecvAction::Park {
            outcome: server, ..
        } = ep.recv(&transport(BASE_B), &rt, None)
        else {
            panic!("recv must park");
        };

        // Последняя capability значка закрывается, когда send уже начат.
        let hold = client.hold_for_operation();
        drop(client);

        let sent = ep.send(&transport(BASE_A).with_badge(BADGE_A), &rt, hold);

        // Сервер видит кадр, а событие об уходе - следующим.
        assert!(matches!(sent, SendAction::Done));
        assert_eq!(server.get(), Some(RendezvousOutcome::Delivered));
        assert!(
            matches!(ep.recv(&transport(BASE_B), &rt, None), RecvAction::SenderGone { badge } if badge == BADGE_A)
        );
    }

    #[test]
    fn an_in_flight_send_queues_no_frame_past_its_badge_departure() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let root = root_capability(&ep);
        let client = badged_capability(&root, BADGE_A);

        let hold = client.hold_for_operation();
        drop(client);

        // Получателя нет: кадр встаёт в очередь, и уход значка снимает его оттуда.
        let SendAction::Park {
            outcome: sender, ..
        } = ep.send(&transport(BASE_A).with_badge(BADGE_A), &rt, hold)
        else {
            panic!("send must park");
        };
        assert_eq!(sender.get(), Some(RendezvousOutcome::PeerGone));

        // Серверу достаётся событие об уходе, кадра снесённой сессии за ним нет.
        assert!(
            matches!(ep.recv(&transport(BASE_B), &rt, None), RecvAction::SenderGone { badge } if badge == BADGE_A)
        );
        assert!(matches!(
            ep.recv(&transport(BASE_B), &rt, None),
            RecvAction::Park { .. }
        ));
    }

    #[test]
    fn an_in_flight_call_delivers_its_frame_before_its_badge_departs() {
        let rt = StubRuntime::arc();
        let ep = Port::new();
        let root = root_capability(&ep);
        let client = badged_capability(&root, BADGE_A);
        let RecvAction::Park {
            outcome: server,
            reply_slot,
            ..
        } = ep.recv(&transport(BASE_B), &rt, None)
        else {
            panic!("recv must park");
        };

        let hold = client.hold_for_operation();
        drop(client);

        let called = ep.call(
            &transport(BASE_A).with_badge(BADGE_A),
            ParkCtx::for_current(&rt),
            hold,
        );

        assert!(matches!(called, CallAction::Park { .. }));
        assert_eq!(server.get(), Some(RendezvousOutcome::Delivered));
        assert!(reply_slot.take().is_some());
    }

    #[test]
    fn port_recv_publishes_departed_badge_into_buffer() {
        use memory::memory_mapper::MemoryMapper;

        let rt = StubRuntime::arc();
        let ep = Port::new();
        let root = root_capability(&ep);
        drop(badged_capability(&root, BADGE_A));

        let mapper = PageMapper::new(BASE_B);
        let table = Arc::new(MutexCell::new(HandleTable::new()));
        let receiver = ThreadTransport::new(mapper.clone(), VirtualAddress::new(BASE_B), table);

        assert_eq!(
            port_recv(&ep, &receiver, &rt, Some(0), None).map(|_| ()),
            Err(IpcError::SenderGone)
        );

        let mut raw = [0u8; 8];
        let badge_offset = core::mem::offset_of!(syscall::IpcBuffer, badge);
        mapper
            .copy_user_in(VirtualAddress::new(BASE_B + badge_offset), &mut raw)
            .expect("read badge back");
        assert_eq!(u64::from_le_bytes(raw), BADGE_A);
    }
}
