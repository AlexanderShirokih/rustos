//! Ожидание завершения команды очереди по линии прерывания транспорта.

use kernel_objects::IrqLine;
use runtime::{Instant, Timeout};
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
pub fn await_completion<M: DeviceMemory>(
    queue: &mut SplitQueue,
    transport: &mut Transport<SlotRegisters>,
    memory: &mut M,
    line: &IrqLine,
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
        line.wait(remaining).map_err(CompletionFailure::Line)?;
        let causes = transport
            .interrupt_status()
            .map_err(CompletionFailure::Slot)?;
        transport
            .acknowledge_interrupt(causes)
            .map_err(CompletionFailure::Slot)?;
        line.ack().map_err(CompletionFailure::Line)?;
    }
}
