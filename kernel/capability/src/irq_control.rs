//! `IrqControl` capability target: range-bounded полномочие минтить `IrqLine`.
//!
//! `Rights::WRITE` на хендле и попадание линии во включительный диапазон `[first_irq, last_irq]`
//! разрешают минт (см. [`irq_mint`](super::irq_mint)). Полномочие делегируется вниз через
//! `TRANSFER`/`DUPLICATE`, как корневой `Resource`. Внутри полномочия лежит ссылка на
//! контроллер, через который привязываются минтимые линии.

use alloc::sync::Arc;

use super::irq_runtime::InterruptsControl;

pub struct IrqControl {
    interrupts: Arc<dyn InterruptsControl>,
    first_irq: u16,
    last_irq: u16,
}

impl IrqControl {
    /// Создаёт полномочие на включительный диапазон линий `[first, last]`
    /// контроллера `interrupts`.
    pub fn new(interrupts: Arc<dyn InterruptsControl>, first_irq: u16, last_irq: u16) -> Arc<Self> {
        Arc::new(Self {
            interrupts,
            first_irq,
            last_irq,
        })
    }

    pub(crate) fn interrupts(&self) -> &Arc<dyn InterruptsControl> {
        &self.interrupts
    }

    /// Разрешён ли минт линии `irq` этим полномочием.
    pub fn permits(&self, irq: u16) -> bool {
        self.first_irq <= irq && irq <= self.last_irq
    }
}

impl core::fmt::Debug for IrqControl {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("IrqControl")
            .field("first_irq", &self.first_irq)
            .field("last_irq", &self.last_irq)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::{super::irq_runtime::StubInterruptsControl, *};

    fn control(first_irq: u16, last_irq: u16) -> Arc<IrqControl> {
        IrqControl::new(Arc::new(StubInterruptsControl), first_irq, last_irq)
    }

    #[test]
    fn permits_within_inclusive_band() {
        let control = control(32, 1019);
        assert!(control.permits(32));
        assert!(control.permits(1019));
        assert!(control.permits(100));
    }

    #[test]
    fn rejects_outside_band() {
        let control = control(32, 64);
        assert!(!control.permits(31));
        assert!(!control.permits(65));
    }

    #[test]
    fn single_line_band() {
        let control = control(48, 48);
        assert!(control.permits(48));
        assert!(!control.permits(47));
        assert!(!control.permits(49));
    }
}
