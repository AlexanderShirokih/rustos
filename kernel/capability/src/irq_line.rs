//! `IrqLine` capability target: привязанная аппаратная линия прерывания.
//!
//! Носит «сырой» номер линии, bound-[`Signal`] (бит `SIGNALED`), состояние
//! ARMED/FIRED и RAII-токен развязки. Доставка переиспользует `Signal`:
//! объект ожидается напрямую через
//! [`as_waitable`](super::CapabilityTarget::as_waitable), отдельный
//! `Signal`-хендл наружу не выдаётся, поэтому подделать срабатывание нельзя.
//!
//! Уровневый протокол (линии GIC level-sensitive): при срабатывании линия
//! маскируется СИНХРОННО (до возврата из IRQ-обработчика) и поднимается
//! `SIGNALED`; [`ack`](IrqLine::ack) снимает latch и размаскирует линию.

use alloc::sync::{Arc, Weak};
use core::sync::atomic::{AtomicU32, Ordering};

use collections::{LockCell, MutexCell};

use super::{
    errors::IpcError,
    irq_runtime::{InterruptsControl, IrqBindToken, IrqSink},
    signal::{SIGNALED, Signal},
};

/// Линия вооружена и ждёт срабатывания.
const ARMED: u32 = 0;
/// Линия сработала и замаскирована; ждёт `ack`.
const FIRED: u32 = 1;

pub struct IrqLine {
    irq: u16,
    signal: Arc<Signal>,
    state: AtomicU32,
    control: Arc<dyn InterruptsControl>,
    bound: MutexCell<Option<IrqBindToken>>,
}

impl IrqLine {
    /// Привязывает линию `irq` через `control` и возвращает готовый объект.
    /// При срабатывании линии вызывается [`on_fire`](Self::on_fire). На ошибке
    /// привязки объект дропается, не оставляя зависшей линии.
    pub fn bind(control: Arc<dyn InterruptsControl>, irq: u16) -> Result<Arc<Self>, IpcError> {
        let line = Arc::new(Self {
            irq,
            signal: Signal::new(),
            state: AtomicU32::new(ARMED),
            control,
            bound: MutexCell::new(None),
        });
        let sink: Arc<dyn IrqSink> = Arc::new(LineSink {
            line: Arc::downgrade(&line),
        });
        let token = line.control.bind_line(irq, sink)?;
        line.bound.with_lock(|slot| *slot = Some(token));
        Ok(line)
    }

    /// Вызывается из IRQ-контекста при срабатывании. Маскирует линию
    /// СИНХРОННО (до `signal` и до возврата в вектор), затем поднимает
    /// `SIGNALED`. Повторное срабатывание до `ack` коалесится: линия уже
    /// замаскирована, бит уже взведён.
    pub(crate) fn on_fire(&self) {
        if self
            .state
            .compare_exchange(ARMED, FIRED, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        // Маска ДО сигнала: к моменту пробуждения драйвера линия тиха
        // (для level-линии это обязательно, иначе interrupt storm).
        self.control.mask(self.irq);
        self.signal.signal(SIGNALED, 0);
    }

    /// Подтверждает обработку: снимает latch `SIGNALED`, затем размаскирует
    /// линию. Вне состояния FIRED — `WrongType` (нечего подтверждать; защита
    /// от двойного `unmask`).
    pub fn ack(&self) -> Result<(), IpcError> {
        if self
            .state
            .compare_exchange(FIRED, ARMED, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(IpcError::WrongType);
        }
        // Снять latch ДО unmask: пока линия замаскирована, потерять
        // следующее срабатывание невозможно.
        self.signal.signal(0, SIGNALED);
        self.control.unmask(self.irq);
        Ok(())
    }

    /// bound-`Signal` для ожидания (через `as_waitable`).
    pub fn event_signal(&self) -> Arc<Signal> {
        self.signal.clone()
    }
}

/// Мост IRQ-обработчик -> `IrqLine`. Держит `Weak`, поэтому срабатывание в
/// гонке с дропом линии становится no-op.
struct LineSink {
    line: Weak<IrqLine>,
}

impl IrqSink for LineSink {
    fn fire(&self) {
        if let Some(line) = self.line.upgrade() {
            line.on_fire();
        }
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::Mutex;

    use super::*;

    /// Контроллер-мок: считает mask/unmask и хранит sink для имитации железа.
    struct MockControl {
        masks: AtomicU32,
        unmasks: AtomicU32,
        sink: Mutex<Option<Arc<dyn IrqSink>>>,
    }

    impl MockControl {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                masks: AtomicU32::new(0),
                unmasks: AtomicU32::new(0),
                sink: Mutex::new(None),
            })
        }

        fn fire(&self) {
            let sink = self.sink.lock().unwrap().clone();
            if let Some(sink) = sink {
                sink.fire();
            }
        }

        fn masks(&self) -> u32 {
            self.masks.load(Ordering::Acquire)
        }

        fn unmasks(&self) -> u32 {
            self.unmasks.load(Ordering::Acquire)
        }
    }

    impl InterruptsControl for MockControl {
        fn bind_line(&self, _irq: u16, sink: Arc<dyn IrqSink>) -> Result<IrqBindToken, IpcError> {
            *self.sink.lock().unwrap() = Some(sink);
            Ok(IrqBindToken::new(()))
        }

        fn mask(&self, _irq: u16) {
            self.masks.fetch_add(1, Ordering::AcqRel);
        }

        fn unmask(&self, _irq: u16) {
            self.unmasks.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[test]
    fn fire_masks_then_latches_and_ack_clears_and_unmasks() {
        let control = MockControl::new();
        let line = IrqLine::bind(control.clone(), 42).expect("bind ok");
        assert_eq!(line.event_signal().peek() & SIGNALED, 0);

        control.fire();
        assert_eq!(control.masks(), 1, "fire masks the line");
        assert_eq!(
            line.event_signal().peek() & SIGNALED,
            SIGNALED,
            "fire latches SIGNALED"
        );

        line.ack().expect("ack ok");
        assert_eq!(
            line.event_signal().peek() & SIGNALED,
            0,
            "ack clears the latch"
        );
        assert_eq!(control.unmasks(), 1, "ack unmasks the line");
    }

    #[test]
    fn double_fire_before_ack_coalesces_to_single_mask() {
        let control = MockControl::new();
        let _line = IrqLine::bind(control.clone(), 42).expect("bind ok");
        control.fire();
        control.fire();
        assert_eq!(control.masks(), 1, "second fire before ack coalesces");
    }

    #[test]
    fn ack_without_fire_is_wrong_type_and_no_unmask() {
        let control = MockControl::new();
        let line = IrqLine::bind(control.clone(), 42).expect("bind ok");
        assert_eq!(line.ack(), Err(IpcError::WrongType));
        assert_eq!(control.unmasks(), 0);
    }

    #[test]
    fn fire_after_drop_is_noop() {
        let control = MockControl::new();
        {
            let _line = IrqLine::bind(control.clone(), 42).expect("bind ok");
        }
        // Линия дропнута; Weak в sink не апгрейдится -> fire ничего не делает.
        control.fire();
        assert_eq!(control.masks(), 0, "fire after drop must not mask");
    }

    #[test]
    fn dropping_line_runs_unbind_token() {
        static DROPPED: AtomicBool = AtomicBool::new(false);

        struct Probe;
        impl Drop for Probe {
            fn drop(&mut self) {
                DROPPED.store(true, Ordering::Release);
            }
        }

        struct ProbeControl;
        impl InterruptsControl for ProbeControl {
            fn bind_line(
                &self,
                _irq: u16,
                _sink: Arc<dyn IrqSink>,
            ) -> Result<IrqBindToken, IpcError> {
                Ok(IrqBindToken::new(Probe))
            }
            fn mask(&self, _irq: u16) {}
            fn unmask(&self, _irq: u16) {}
        }

        let line = IrqLine::bind(Arc::new(ProbeControl), 7).expect("bind ok");
        assert!(!DROPPED.load(Ordering::Acquire));
        drop(line);
        assert!(
            DROPPED.load(Ordering::Acquire),
            "dropping IrqLine must drop the bind token"
        );
    }
}
