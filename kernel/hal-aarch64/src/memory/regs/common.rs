//! Общие типы и макросы для системных регистров.

/// Маркер уровня исключения EL1.
pub enum EL1 {}

/// Читает системный регистр AArch64 через инструкцию `mrs`.
///
/// Возвращает значение регистра как `u64`. Должен использоваться внутри `unsafe` блока.
///
/// # Пример
/// ```rust
/// // SAFETY: Чтение SCTLR_EL1 допустимо на EL1.
/// let value = unsafe { read_sysreg!(sctlr_el1) };
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
/// ```rust
/// // SAFETY: Запись в TTBR0_EL1 допустима на EL1 при корректном выравнивании адреса.
/// unsafe { write_sysreg!(ttbr0_el1, root_addr) };
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

/// Объединяет биты из массива в одно значение u64.
#[macro_export]
macro_rules! combine_bits {
    ($bits:expr $(,)?) => {{
        let bits = $bits;
        let mut out: u64 = 0;
        let mut i: usize = 0;
        while i < bits.len() {
            out |= bits[i].encode();
            i += 1;
        }
        out
    }};
}
