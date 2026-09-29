//! `IrqLine` capability target: привязанная аппаратная линия прерывания.
//!
//! Содержит "сырой" номер линии, bound-[`Signal`] (бит `SIGNALED`), состояние
//! ARMED/FIRED и RAII-токен развязки. Доставка переиспользует `Signal`:
//! объект ожидается напрямую через
//! [`as_waitable`](super::CapabilityTarget::as_waitable), отдельный
//! `Signal`-хендл наружу не выдаётся, и подделать срабатывание нельзя.
//!
//! Уровневый протокол (линии GIC level-sensitive). При срабатывании линия маскируется
//! синхронно (до возврата из IRQ-обработчика) и поднимается `SIGNALED`. [`ack`](IrqLine::ack)
//! снимает latch и размаскирует линию.

use alloc::sync::{Arc, Weak};
use core::sync::atomic::{AtomicU32, Ordering};

use collections::{LockCell, MutexCell};
use syscall::IrqTrigger;

use super::{
    errors::IpcError,
    irq_runtime::{InterruptsControl, IrqBindToken, IrqSink},
    rev_node::RevocationHook,
    signal::{SIGNALED, Signal},
};

const ARMED: u32 = 0;
const FIRED: u32 = 1;

enum Binding {
    Pending,
    Bound(#[allow(dead_code)] IrqBindToken),
    Unbound,
}

pub struct IrqLine {
    irq: u16,
    signal: Arc<Signal>,
    state: AtomicU32,
    control: Arc<dyn InterruptsControl>,
    bound: MutexCell<Binding>,
}

impl IrqLine {
    /// Привязывает линию `irq` с триггером `trigger` через `control` и
    /// возвращает готовый объект.
    ///
    /// С этого момента срабатывание линии маскирует её на контроллере и поднимает `SIGNALED` на
    /// сигнале, который отдаёт [`event_signal`](Self::event_signal). Занятая линия отвергается с
    /// [`IpcError::AlreadyBound`]. На отказе привязки объект разрушается здесь же, и линия на
    /// контроллере остаётся свободной. Привязка длится до [`RevocationHook::revoke`] либо до
    /// разрушения объекта.
    pub fn bind(
        control: Arc<dyn InterruptsControl>,
        irq: u16,
        trigger: IrqTrigger,
    ) -> Result<Arc<Self>, IpcError> {
        let line = Arc::new(Self {
            irq,
            signal: Signal::new(),
            state: AtomicU32::new(ARMED),
            control,
            bound: MutexCell::new(Binding::Pending),
        });
        let sink: Arc<dyn IrqSink> = Arc::new(LineSink {
            line: Arc::downgrade(&line),
        });
        let token = line.control.bind_line(irq, trigger, sink)?;
        line.bound.with_lock(|bound| *bound = Binding::Bound(token));
        Ok(line)
    }

    pub(crate) fn on_fire(&self) {
        let fired = self.bound.with_lock(|bound| {
            // Срабатывание в ходе привязки тоже маскирует линию.
            if matches!(bound, Binding::Unbound) {
                return false;
            }

            if self
                .state
                .compare_exchange(ARMED, FIRED, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return false;
            }

            // Драйвер просыпается при уже замаскированной линии.
            self.control.mask(self.irq);
            true
        });

        if fired {
            self.signal.signal(SIGNALED, 0);
        }
    }

    /// Подтверждает обработку. Снимает latch `SIGNALED`, затем размаскирует
    /// линию. Вне состояния FIRED - `WrongType` (нечего подтверждать, защита
    /// от двойного `unmask`). После отвязки линии - [`IpcError::Revoked`] без снятия маски.
    pub fn ack(&self) -> Result<(), IpcError> {
        self.bound.with_lock(|bound| {
            if !matches!(bound, Binding::Bound(_)) {
                return Err(IpcError::Revoked);
            }

            if self
                .state
                .compare_exchange(FIRED, ARMED, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return Err(IpcError::WrongType);
            }

            // Latch снимается до снятия маски, и следующее срабатывание не теряется.
            self.signal.signal(0, SIGNALED);
            self.control.unmask(self.irq);
            Ok(())
        })
    }

    /// bound-`Signal` для ожидания (через `as_waitable`). Бит `SIGNALED` поднят с момента
    /// срабатывания линии и до [`ack`](Self::ack). Хендлом наружу этот сигнал не выдаётся.
    pub fn event_signal(&self) -> Arc<Signal> {
        self.signal.clone()
    }
}

impl RevocationHook for IrqLine {
    fn revoke(&self) {
        // Под локом прерывания на этом CPU замаскированы до отключения линии на контроллере.
        self.bound.with_lock(|bound| *bound = Binding::Unbound);
    }
}

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
    use core::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;

    use super::*;

    struct UnbindProbe(Arc<AtomicU32>);

    impl Drop for UnbindProbe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    struct MockControl {
        masks: AtomicU32,
        unmasks: AtomicU32,
        unbinds: Arc<AtomicU32>,
        fire_on_bind: bool,
        sink: Mutex<Option<Arc<dyn IrqSink>>>,
    }

    impl MockControl {
        fn new() -> Arc<Self> {
            Self::with_fire_on_bind(false)
        }

        fn with_fire_on_bind(fire_on_bind: bool) -> Arc<Self> {
            Arc::new(Self {
                masks: AtomicU32::new(0),
                unmasks: AtomicU32::new(0),
                unbinds: Arc::new(AtomicU32::new(0)),
                fire_on_bind,
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

        fn unbinds(&self) -> u32 {
            self.unbinds.load(Ordering::Acquire)
        }
    }

    impl InterruptsControl for MockControl {
        fn bind_line(
            &self,
            _irq: u16,
            _trigger: IrqTrigger,
            sink: Arc<dyn IrqSink>,
        ) -> Result<IrqBindToken, IpcError> {
            if self.fire_on_bind {
                sink.fire();
            }
            *self.sink.lock().unwrap() = Some(sink);
            Ok(IrqBindToken::new(UnbindProbe(self.unbinds.clone())))
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
        let line = IrqLine::bind(control.clone(), 42, IrqTrigger::Level).expect("bind ok");
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
        let _line = IrqLine::bind(control.clone(), 42, IrqTrigger::Level).expect("bind ok");
        control.fire();
        control.fire();
        assert_eq!(control.masks(), 1, "second fire before ack coalesces");
    }

    #[test]
    fn ack_without_fire_is_wrong_type_and_no_unmask() {
        let control = MockControl::new();
        let line = IrqLine::bind(control.clone(), 42, IrqTrigger::Level).expect("bind ok");
        assert_eq!(line.ack(), Err(IpcError::WrongType));
        assert_eq!(control.unmasks(), 0);
    }

    #[test]
    fn fire_after_drop_is_noop() {
        let control = MockControl::new();
        {
            let _line = IrqLine::bind(control.clone(), 42, IrqTrigger::Level).expect("bind ok");
        }
        control.fire();
        assert_eq!(control.masks(), 0, "fire after drop must not mask");
    }

    #[test]
    fn dropping_line_runs_unbind_token() {
        let control = MockControl::new();
        let line = IrqLine::bind(control.clone(), 7, IrqTrigger::Level).expect("bind ok");
        assert_eq!(control.unbinds(), 0);

        drop(line);

        assert_eq!(
            control.unbinds(),
            1,
            "dropping IrqLine must drop the bind token"
        );
    }

    #[test]
    fn revoke_drops_the_token_of_a_live_line_once() {
        let control = MockControl::new();
        let line = IrqLine::bind(control.clone(), 7, IrqTrigger::Level).expect("bind ok");

        line.revoke();
        assert_eq!(control.unbinds(), 1, "revoke drops the bind token");

        line.revoke();
        drop(line);
        assert_eq!(
            control.unbinds(),
            1,
            "repeat revoke and drop leave the token alone"
        );
    }

    #[test]
    fn ack_after_revoke_is_revoked_and_keeps_the_mask() {
        let control = MockControl::new();
        let line = IrqLine::bind(control.clone(), 42, IrqTrigger::Level).expect("bind ok");
        control.fire();

        line.revoke();

        assert_eq!(line.ack(), Err(IpcError::Revoked));
        assert_eq!(control.unmasks(), 0, "ack after revoke must not unmask");
    }

    #[test]
    fn fire_after_revoke_neither_masks_nor_signals() {
        let control = MockControl::new();
        let line = IrqLine::bind(control.clone(), 42, IrqTrigger::Level).expect("bind ok");

        line.revoke();
        control.fire();

        assert_eq!(control.masks(), 0, "fire after revoke must not mask");
        assert_eq!(line.event_signal().peek() & SIGNALED, 0);
    }

    #[test]
    fn fire_inside_bind_masks_and_ack_after_bind_unmasks() {
        let control = MockControl::with_fire_on_bind(true);
        let line = IrqLine::bind(control.clone(), 42, IrqTrigger::Level).expect("bind ok");

        assert_eq!(control.masks(), 1, "fire inside bind masks the line");
        assert_eq!(line.event_signal().peek() & SIGNALED, SIGNALED);

        line.ack().expect("ack ok");
        assert_eq!(control.unmasks(), 1, "ack after bind unmasks the line");
    }
}
