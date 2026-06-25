//! Архитектурно-независимый мост к контроллеру прерываний — sibling [`KernelRuntime`].
//!
//! Capability-крейт не зависит от `drivers-common`: он говорит «сырыми» `u16`
//! номерами линий через [`InterruptsControl`], а kernelspace ставит адаптер,
//! форвардящий вызовы в `InterruptsService`. Так IRQ-capability остаётся
//! архитектурно-независимой и юнит-тестируемой с мок-контроллером.

use alloc::{boxed::Box, sync::Arc};

use spin::Once;

use super::errors::IpcError;

/// Приёмник аппаратного прерывания: мост к [`IrqLine::on_fire`](super::IrqLine).
/// Реализация держит `Weak<IrqLine>`, поэтому срабатывание в гонке с закрытием
/// линии становится no-op.
pub trait IrqSink: Send + Sync {
    /// Вызывается из IRQ-контекста при срабатывании линии.
    fn fire(&self);
}

/// Контроллер прерываний с точки зрения capability-слоя: привязка линии к
/// `sink` и маск/анмаск по «сырому» номеру. Ставится один раз в kmain.
pub trait InterruptsControl: Send + Sync {
    /// Привязывает линию `irq` так, что её срабатывания зовут `sink.fire()`,
    /// и возвращает RAII-токен, чей drop снимает привязку (и маскирует линию).
    fn bind_line(&self, irq: u16, sink: Arc<dyn IrqSink>) -> Result<IrqBindToken, IpcError>;

    /// Маскирует (запрещает) линию на контроллере.
    fn mask(&self, irq: u16);

    /// Снимает маску (разрешает) линию на контроллере.
    fn unmask(&self, irq: u16);
}

/// Непрозрачный RAII-развязчик линии. Дроп токена снимает привязку: на стороне
/// kernelspace это дропает `IrqBound`, чей cleanup отключает линию и удаляет
/// обработчик. Токен владеется единственным `Arc<IrqLine>` — `IrqBound` (FnOnce)
/// нельзя клонировать, поэтому несколько хендлов разделяют линию через `Arc`.
pub struct IrqBindToken(#[allow(dead_code)] Box<dyn Send>);

impl IrqBindToken {
    /// Оборачивает произвольный `Send`-гард; его `Drop` исполняется при дропе токена.
    pub fn new<G: Send + 'static>(guard: G) -> Self {
        Self(Box::new(guard))
    }
}

static IRQ_CONTROL: Once<Arc<dyn InterruptsControl>> = Once::new();

/// Ставит глобальный контроллер прерываний (один раз, в kmain).
pub fn install_interrupts_control(control: Arc<dyn InterruptsControl>) {
    assert!(
        IRQ_CONTROL.get().is_none(),
        "InterruptsControl is already installed"
    );
    let _ = IRQ_CONTROL.call_once(|| control);
}

/// Доступ к глобальному контроллеру; паникует, если он ещё не установлен.
pub fn interrupts_control() -> &'static Arc<dyn InterruptsControl> {
    IRQ_CONTROL
        .get()
        .expect("InterruptsControl not installed; install_interrupts_control() must run in kmain")
}
