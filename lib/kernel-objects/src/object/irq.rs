//! Типизированные обёртки над IRQ-полномочием и линией прерывания ядра.

use syscall::{IrqTrigger, SIGNALED, Timeout, kind};

use crate::{
    error::{Error, Result, unit},
    handle::OwnedHandle,
    svc,
};

/// Владеет хэндлом полномочия `IrqControl` и закрывает его на drop.
pub type IrqControl = OwnedHandle<kind::IrqControl>;

/// Владеет хэндлом линии прерывания и закрывает его на drop. Закрытие снимает
/// биндинг линии в ядре.
pub type IrqLine = OwnedHandle<kind::IrqLine>;

impl OwnedHandle<kind::IrqControl> {
    /// Минтит линию `irq` с триггером `trigger` по полномочию. Требует
    /// `Rights::WRITE` и попадания `irq` в диапазон полномочия. Уже занятая
    /// линия - `AlreadyBound`.
    pub fn mint(&self, irq: u16, trigger: IrqTrigger) -> Result<IrqLine> {
        svc::irq_mint(self.as_raw(), irq, trigger)
            .map(IrqLine::from_handle)
            .map_err(Error::Syscall)
    }
}

impl OwnedHandle<kind::IrqLine> {
    /// Ждёт срабатывания линии до `timeout`. Возвращает наблюдаемую маску,
    /// тайм-аут - `Err`.
    pub fn wait(&self, timeout: Timeout) -> Result<u32> {
        super::wait_signals(self.as_raw(), SIGNALED, timeout)
    }

    /// Снимает latch `SIGNALED` и размаскирует линию, подтверждая тем самым прерывание.
    /// Требует `Rights::WRITE`.
    pub fn ack(&self) -> Result<()> {
        unit(svc::irq_ack(self.as_raw()))
    }
}
