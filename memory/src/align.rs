//! Утилиты выравнивания адресов и размеров.

/// Выравнивает значение вверх до `align`.
///
/// Требует `align` как степень двойки.
pub const fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

/// Выравнивает значение вверх до `align` с проверками.
pub fn align_up_checked(value: usize, align: usize) -> Option<usize> {
    if align == 0 || !align.is_power_of_two() {
        return None;
    }
    value.checked_add(align - 1).map(|v| v & !(align - 1))
}
