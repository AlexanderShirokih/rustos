//! Аппаратное состояние ARM Generic Timer (virtual timer, `CNTV_*`).

use alloc::{string::String, sync::Arc};

use drivers_common::services::timer::{TickHandler, deadline_ticks, ticks_to_ns};
use spin::Once;

use crate::{read_sysreg, write_sysreg};

pub(super) const CNTV_CTL_ENABLE: u32 = 1 << 0;

pub(super) struct ArmGenericTimerState {
    pub(super) frequency: u64,
    pub(super) handler: Once<Arc<dyn TickHandler>>,
}

impl ArmGenericTimerState {
    pub(super) fn new() -> Result<Self, String> {
        let frequency = Self::read_cntfrq_el0();

        if frequency == 0 {
            return Err("CNTFRQ_EL0 returned zero frequency".into());
        }

        Ok(Self {
            frequency,
            handler: Once::new(),
        })
    }

    pub(super) fn set_handler(&self, handler: Arc<dyn TickHandler>) {
        assert!(
            self.handler.get().is_none(),
            "TickHandler is already registered"
        );
        let _ = self.handler.call_once(|| handler);
    }

    pub(super) fn schedule_next(&self, deadline_ns: u64) {
        // Компаратор абсолютный и 64-битный. Относительная запись `CNTV_TVAL_EL0`
        // расширяется знаком, и срок дальше 2^31 тиков попадал бы в прошлое.
        Self::write_cntv_cval_el0(deadline_ticks(deadline_ns, self.frequency));
        Self::write_cntv_ctl_el0(CNTV_CTL_ENABLE);
    }

    pub(super) fn get_elapsed_ns(&self) -> u64 {
        ticks_to_ns(Self::read_cntvct_el0(), self.frequency)
    }

    pub(super) fn on_interrupt(&self) {
        if let Some(handler) = self.handler.get() {
            handler.on_tick(self.get_elapsed_ns());
        }
    }

    fn read_cntfrq_el0() -> u64 {
        // SAFETY: Чтение системного регистра CNTFRQ_EL0 разрешено на EL1 при корректной
        // конфигурации платформы и не нарушает инварианты памяти.
        unsafe { read_sysreg!(cntfrq_el0) }
    }

    fn read_cntvct_el0() -> u64 {
        // SAFETY: Чтение CNTVCT_EL0 является побочным только по времени и не модифицирует
        // память/состояние, влияющее на безопасность Rust-кода.
        unsafe { read_sysreg!(cntvct_el0) }
    }

    fn write_cntv_cval_el0(value: u64) {
        // SAFETY: Запись в CNTV_CVAL_EL0 программирует абсолютное значение CNTVCT_EL0,
        // на котором срабатывает virtual timer, и не затрагивает память. ISB гарантирует,
        // что следующая инструкция выполняется уже с обновлённым значением регистра.
        unsafe {
            write_sysreg!(cntv_cval_el0, value);
            core::arch::asm!("isb", options(nomem, nostack, preserves_flags));
        }
    }

    fn write_cntv_ctl_el0(value: u32) {
        // SAFETY: Запись в CNTV_CTL_EL0 меняет только биты управления virtual timer.
        // Используются только документированные значения (enable/unmask). ISB гарантирует
        // немедленное применение изменений управляющего регистра.
        unsafe {
            write_sysreg!(cntv_ctl_el0, u64::from(value));
            core::arch::asm!("isb", options(nomem, nostack, preserves_flags));
        }
    }
}
