//! Аппаратное состояние ARM Generic Timer.

use alloc::{string::String, sync::Arc};
use core::cmp;

use drivers_common::services::timer::TickHandler;
use spin::Once;

use crate::{read_sysreg, write_sysreg};

pub(super) const CNTP_CTL_ENABLE: u32 = 1 << 0;

/// Runtime-состояние ARM Generic Timer.
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
        let now_ns = self.get_elapsed_ns();
        let delta_ns = deadline_ns.saturating_sub(now_ns);
        let reload_value = Self::compute_counter_value(self.frequency, delta_ns).unwrap_or(1);

        Self::write_cntp_tval_el0(reload_value);
        Self::write_cntp_ctl_el0(CNTP_CTL_ENABLE);
    }

    pub(super) fn get_elapsed_ns(&self) -> u64 {
        Self::ticks_to_ns(Self::read_cntpct_el0(), self.frequency)
    }

    fn ticks_to_ns(ticks: u64, frequency: u64) -> u64 {
        (u128::from(ticks) * 1_000_000_000 / u128::from(frequency)) as u64
    }

    pub(super) fn on_interrupt(&self) {
        if let Some(handler) = self.handler.get() {
            handler.on_tick(self.get_elapsed_ns());
        }
    }

    fn compute_counter_value(frequency: u64, interval_ns: u64) -> Option<u32> {
        let ticks = frequency.checked_mul(interval_ns)? / 1_000_000_000;

        if ticks == 0 {
            return None;
        }

        Some(cmp::min(ticks, u64::from(u32::MAX)) as u32)
    }

    fn read_cntfrq_el0() -> u64 {
        // SAFETY: Чтение системного регистра CNTFRQ_EL0 разрешено на EL1 при корректной
        // конфигурации платформы и не нарушает инварианты памяти.
        unsafe { read_sysreg!(cntfrq_el0) }
    }

    fn read_cntpct_el0() -> u64 {
        // SAFETY: Чтение CNTPCT_EL0 является побочным только по времени и не модифицирует
        // память/состояние, влияющее на безопасность Rust-кода.
        unsafe { read_sysreg!(cntpct_el0) }
    }

    fn write_cntp_tval_el0(value: u32) {
        // SAFETY: Запись в CNTP_TVAL_EL0 программирует относительный дедлайн физического таймера.
        // Аппаратно устанавливает CNTP_CVAL_EL0 = CNTPCT_EL0 + TVAL. ISB гарантирует
        // что следующая инструкция видит актуальное значение таймера.
        unsafe {
            write_sysreg!(cntp_tval_el0, u64::from(value));
            core::arch::asm!("isb", options(nomem, nostack, preserves_flags));
        }
    }

    fn write_cntp_ctl_el0(value: u32) {
        // SAFETY: Запись в CNTP_CTL_EL0 меняет только биты управления физического таймера.
        // Используются только документированные значения (enable/unmask). ISB гарантирует
        // немедленное применение изменений управляющего регистра.
        unsafe {
            write_sysreg!(cntp_ctl_el0, u64::from(value));
            core::arch::asm!("isb", options(nomem, nostack, preserves_flags));
        }
    }
}
