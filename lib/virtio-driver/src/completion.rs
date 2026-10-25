//! Ожидание завершения команды очереди по линии прерывания транспорта.

use executor::Reactor;
use kernel_objects::IrqLine;
use runtime::{Instant, Timeout};
use syscall::{SIGNALED, SyscallError, WaitItem};
use virtio::{Completion, DeviceMemory, QueueError, SplitQueue, Transport};

use crate::registers::{SlotFailure, SlotRegisters};

/// Отказ ожидания завершения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionFailure {
    /// Отказ очереди.
    Queue(QueueError<SlotFailure>),
    /// Отказ регистров слота.
    Slot(SlotFailure),
    /// Отказ ожидания линии. Истёкший срок - `Syscall(Timeout)`.
    Line(kernel_objects::Error),
}

/// Возвращает очередной использованный элемент очереди `queue`, дождавшись его не дольше
/// `timeout` от вызова. Прерывания транспорта, пришедшие за это время, подтверждаются.
///
/// Отказы:
/// - `Line(Syscall(Timeout))` - срок истёк;
/// - `Line` - отказ ожидания или подтверждения линии `line`;
/// - `Queue` и `Slot` - отказ чтения очереди либо регистров прерывания.
pub async fn await_completion<M: DeviceMemory>(
    queue: &mut SplitQueue,
    transport: &mut Transport<SlotRegisters>,
    memory: &mut M,
    line: &IrqLine,
    reactor: &Reactor,
    timeout: Timeout,
) -> Result<Completion, CompletionFailure> {
    let start = Instant::now();

    loop {
        if let Some(completion) = queue
            .take_used(transport, memory)
            .map_err(CompletionFailure::Queue)?
        {
            return Ok(completion);
        }

        let remaining = timeout.saturating_sub(Instant::now().since(start));
        reactor
            .timeout(
                remaining,
                reactor.wait(WaitItem::new(line.as_raw(), SIGNALED)),
            )
            .await
            .ok_or(CompletionFailure::Line(kernel_objects::Error::Syscall(
                SyscallError::Timeout,
            )))?
            .map_err(CompletionFailure::Line)?;

        let causes = transport
            .interrupt_status()
            .await
            .map_err(CompletionFailure::Slot)?;
        transport
            .acknowledge_interrupt(causes)
            .await
            .map_err(CompletionFailure::Slot)?;

        line.ack().map_err(CompletionFailure::Line)?;
    }
}
