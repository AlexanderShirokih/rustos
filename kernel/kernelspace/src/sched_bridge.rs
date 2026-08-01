//! Мост между загрузочным путём CPU и планировщиком.
//!
//! Вторичный CPU входит в планировщик со своего загрузочного пути в
//! платформенном слое, а сам планировщик собирается здесь. Мост даёт первому
//! добраться до второго, не связывая крейты типами планировщика.

use alloc::{boxed::Box, sync::Arc};

use drivers_common::services::interrupts::{
    CpuMask, InterruptsService, IpiTarget, IrqBinding, IrqBound, IrqHandler, IrqNumber,
    IrqPriority, IrqRegistrationError,
};
use klog::warn;
use scheduler::{CpuBootstrapError, CpuEntry};
use spin::{Mutex, Once};

/// SGI, которым CPU просят перевыбрать поток.
///
/// Номер из диапазона межпроцессорных прерываний; конкретное значение выбрано
/// свободным - других SGI ядро не использует.
pub const RESCHEDULE_IPI: IrqNumber = IrqNumber::new(0);

static CPU_ENTRY: Once<Arc<dyn CpuEntry>> = Once::new();

static RESCHEDULE_BINDING: Mutex<Option<IrqBound>> = Mutex::new(None);

/// Публикует вход в планировщик. Вызывается один раз, при его подъёме на boot
/// CPU и до старта вторичных.
pub fn install_cpu_entry(entry: Arc<dyn CpuEntry>) {
    let _ = CPU_ENTRY.call_once(|| entry);
}

/// Создаёт per-CPU состояние планировщика на текущем CPU.
pub fn bootstrap_this_cpu() -> Result<(), CpuBootstrapError> {
    CPU_ENTRY
        .get()
        .ok_or(CpuBootstrapError::NotReady)?
        .bootstrap()
}

/// Отдаёт текущий CPU планировщику. Возвращается, только если планировщик не
/// опубликован: сам вход не возвращается.
pub fn enter_scheduler() -> CpuBootstrapError {
    match CPU_ENTRY.get() {
        Some(entry) => entry.enter(),
        None => CpuBootstrapError::NotReady,
    }
}

/// Привязывает reschedule-IPI к обработчику, перевыбирающему поток.
///
/// Линия приватна для CPU: контроллер включает её на каждом CPU в его per-CPU
/// инициализации, и привязка предшествует подъёму вторичных.
pub fn bind_reschedule_ipi(
    interrupts: &Arc<dyn InterruptsService>,
    entry: Arc<dyn RescheduleTarget>,
) -> Result<(), IrqRegistrationError> {
    let binding = IrqBinding::new(
        RESCHEDULE_IPI,
        // Триггер SGI задан архитектурой, менять его нечем.
        None,
        // Перевыбор потока не должен ждать за устройственными линиями.
        IrqPriority::HIGHEST,
        // Адресация SGI идёт полем самой рассылки, а не маршрутизацией линии.
        CpuMask::ALL,
        Box::new(RescheduleHandler { entry }),
    );

    *RESCHEDULE_BINDING.lock() = Some(interrupts.bind(binding)?);
    Ok(())
}

/// Тот, кто умеет перевыбрать поток на текущем CPU.
pub trait RescheduleTarget: Send + Sync {
    fn on_reschedule(&self);
}

struct RescheduleHandler {
    entry: Arc<dyn RescheduleTarget>,
}

impl IrqHandler for RescheduleHandler {
    fn handle(&self) {
        self.entry.on_reschedule();
    }
}

/// Просит `target` перевыбрать поток.
pub fn send_reschedule(target: IpiTarget) {
    if let Err(error) = crate::irq_bridge::send_ipi(RESCHEDULE_IPI, target) {
        warn!("Reschedule IPI was not delivered: {error}");
    }
}
