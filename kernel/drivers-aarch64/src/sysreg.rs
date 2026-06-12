//! Макросы для доступа к системным регистрам AArch64.

/// Читает системный регистр AArch64 через инструкцию `mrs`.
///
/// Возвращает значение регистра как `u64`. Должен использоваться внутри `unsafe` блока.
///
/// # Пример
/// ```no_run
/// #[cfg(target_arch = "aarch64")]
/// {
///     use drivers_aarch64::read_sysreg;
///     // SAFETY: Чтение CNTFRQ_EL0 допустимо на EL1 при корректной конфигурации платформы.
///     let freq = unsafe { read_sysreg!(cntfrq_el0) };
///     let _ = freq;
/// }
/// ```
#[macro_export]
macro_rules! read_sysreg {
    ($reg:ident) => {{
        let mut value: u64;
        ::core::arch::asm!(
            ::core::concat!("mrs {0}, ", ::core::stringify!($reg)),
            out(reg) value,
            options(nomem, nostack, preserves_flags)
        );
        value
    }};
}

/// Записывает значение в системный регистр AArch64 через инструкцию `msr`.
///
/// Значение приводится к `u64` перед записью. Должен использоваться внутри `unsafe` блока.
///
/// # Пример
/// ```no_run
/// #[cfg(target_arch = "aarch64")]
/// {
///     use drivers_aarch64::write_sysreg;
///     const CNTP_CTL_ENABLE: u32 = 1 << 0;
///     // SAFETY: Запись в CNTP_CTL_EL0 меняет только биты управления физического таймера.
///     unsafe { write_sysreg!(cntp_ctl_el0, CNTP_CTL_ENABLE as u64) };
/// }
/// ```
#[macro_export]
macro_rules! write_sysreg {
    ($reg:ident, $value:expr) => {
        ::core::arch::asm!(
            ::core::concat!("msr ", ::core::stringify!($reg), ", {0}"),
            in(reg) ($value as u64),
            options(nomem, nostack, preserves_flags)
        )
    };
}
