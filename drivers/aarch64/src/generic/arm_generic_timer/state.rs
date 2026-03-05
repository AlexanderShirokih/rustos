//! Аппаратное состояние ARM Generic Timer.

use alloc::string::String;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::{read_sysreg, write_sysreg};

pub(super) const CNTP_CTL_ENABLE: u32 = 1 << 0;

/// Runtime-состояние ARM Generic Timer.
pub(super) struct ArmGenericTimerState {
    pub(super) frequency: u64,
    pub(super) reload_value: AtomicU32,
}

impl ArmGenericTimerState {
    pub(super) fn new() -> Result<Self, String> {
        let frequency = Self::read_cntfrq_el0();

        if frequency == 0 {
            return Err("CNTFRQ_EL0 returned zero frequency".into());
        }

        Ok(Self {
            frequency,
            reload_value: AtomicU32::new(0),
        })
    }

    pub(super) fn set_periodic(&self, interval_ms: u64) {
        let reload_value = Self::compute_counter_value(self.frequency, interval_ms).unwrap_or(1);

        self.reload_value.store(reload_value, Ordering::Relaxed);

        Self::write_cntp_tval_el0(reload_value);
        Self::write_cntp_ctl_el0(CNTP_CTL_ENABLE);
    }

    pub(super) fn get_elapsed_ns(&self) -> u64 {
        Self::ticks_to_ns(Self::read_cntpct_el0(), self.frequency)
    }

    fn ticks_to_ns(ticks: u64, frequency: u64) -> u64 {
        (ticks as u128 * 1_000_000_000 / frequency as u128) as u64
    }

    pub(super) fn on_interrupt(&self) {
        klog::debug!("timer tick...");

        let reload_value = self.reload_value.load(Ordering::Relaxed);
        if reload_value != 0 {
            Self::write_cntp_tval_el0(reload_value);
        }
    }

    fn compute_counter_value(frequency: u64, interval_ms: u64) -> Option<u32> {
        let ticks = frequency.checked_mul(interval_ms)? / 1_000;

        if ticks == 0 {
            return None;
        }

        ticks.try_into().ok()
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
            write_sysreg!(cntp_tval_el0, value as u64);
            core::arch::asm!("isb", options(nomem, nostack, preserves_flags));
        }
    }

    fn write_cntp_ctl_el0(value: u32) {
        // SAFETY: Запись в CNTP_CTL_EL0 меняет только биты управления физического таймера.
        // Используются только документированные значения (enable/unmask). ISB гарантирует
        // немедленное применение изменений управляющего регистра.
        unsafe {
            write_sysreg!(cntp_ctl_el0, value as u64);
            core::arch::asm!("isb", options(nomem, nostack, preserves_flags));
        }
    }
}
