//! Типизированные обёртки над IRQ-полномочием и линией прерывания ядра.

use syscall::{SIGNALED, Timeout};

use crate::{
    error::{Error, Result, unit},
    handle::{BorrowedHandle, OwnedHandle},
    svc,
};

/// Владеет хэндлом полномочия `IrqControl` и закрывает его на drop.
#[derive(Debug)]
pub struct IrqControl {
    handle: OwnedHandle,
}

/// Владеет хэндлом линии прерывания и закрывает его на drop. Закрытие снимает
/// биндинг линии в ядре.
#[derive(Debug)]
pub struct IrqLine {
    handle: OwnedHandle,
}

impl IrqControl {
    /// Берёт во владение хэндл `IrqControl`.
    pub fn from_handle(handle: OwnedHandle) -> Self {
        Self { handle }
    }

    /// Заимствование хэндла на время одного вызова.
    pub fn handle(&self) -> BorrowedHandle<'_> {
        self.handle.borrow()
    }

    /// Минтит линию `irq` по полномочию. Требует `Rights::WRITE` и попадания
    /// `irq` в диапазон полномочия; уже занятая линия - `ResourceExhausted`.
    pub fn mint(&self, irq: u16) -> Result<IrqLine> {
        svc::irq_mint(self.handle.as_raw(), irq)
            .map(|handle| IrqLine::from_handle(OwnedHandle::from_handle(handle)))
            .map_err(Error::Syscall)
    }

    /// Отдаёт владеемый хэндл.
    pub fn into_handle(self) -> OwnedHandle {
        self.handle
    }
}

impl IrqLine {
    /// Берёт во владение хэндл `IrqLine`.
    pub fn from_handle(handle: OwnedHandle) -> Self {
        Self { handle }
    }

    /// Заимствование хэндла на время одного вызова.
    pub fn handle(&self) -> BorrowedHandle<'_> {
        self.handle.borrow()
    }

    /// Ждёт срабатывания линии до `timeout`; возвращает наблюдаемую маску,
    /// тайм-аут - `Err`.
    pub fn wait(&self, timeout: Timeout) -> Result<u32> {
        super::wait_signals(self.handle.as_raw(), SIGNALED, timeout)
    }

    /// Подтверждает прерывание: снимает latch `SIGNALED` и размаскирует линию.
    /// Требует `Rights::WRITE`.
    pub fn ack(&self) -> Result<()> {
        unit(svc::irq_ack(self.handle.as_raw()))
    }

    /// Отдаёт владеемый хэндл.
    pub fn into_handle(self) -> OwnedHandle {
        self.handle
    }
}
