//! `IrqControl` capability target: range-bounded полномочие минтить `IrqLine`.
//!
//! Прямой аналог [`Resource`](super::Resource) для физпамяти: `Rights::WRITE`
//! на хендле + попадание линии в включительный диапазон `[first_irq, last_irq]`
//! разрешают минт (см. [`irq_mint`](super::irq_mint)). Полномочие делегируется
//! вниз через `TRANSFER`/`DUPLICATE`, как корневой `Resource`.

use alloc::sync::Arc;

#[derive(Debug)]
pub struct IrqControl {
    first_irq: u16,
    last_irq: u16,
}

impl IrqControl {
    /// Создаёт полномочие на включительный диапазон линий `[first, last]`.
    pub fn new(first_irq: u16, last_irq: u16) -> Arc<Self> {
        Arc::new(Self {
            first_irq,
            last_irq,
        })
    }

    /// Разрешён ли минт линии `irq` этим полномочием.
    pub fn permits(&self, irq: u16) -> bool {
        self.first_irq <= irq && irq <= self.last_irq
    }

    pub fn first_irq(&self) -> u16 {
        self.first_irq
    }

    pub fn last_irq(&self) -> u16 {
        self.last_irq
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permits_within_inclusive_band() {
        let control = IrqControl::new(32, 1019);
        assert!(control.permits(32));
        assert!(control.permits(1019));
        assert!(control.permits(100));
    }

    #[test]
    fn rejects_outside_band() {
        let control = IrqControl::new(32, 64);
        assert!(!control.permits(31));
        assert!(!control.permits(65));
    }

    #[test]
    fn single_line_band() {
        let control = IrqControl::new(48, 48);
        assert!(control.permits(48));
        assert!(!control.permits(47));
        assert!(!control.permits(49));
    }
}
