//! Выравнивание адреса и размера вверх по заданной границе, с проверкой переполнения и без.

/// Требует `align` как степень двойки. Не проверяет переполнение `value + align`.
/// Переполнение суммы даёт неверный результат.
pub const fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

/// Выравнивает `value` вверх до `align`, как [`align_up`], но проверяет входные данные.
/// Возвращает `None`, если `align` не степень двойки (включая ноль) либо `value + align`
/// переполняет `usize`.
pub fn align_up_checked(value: usize, align: usize) -> Option<usize> {
    if align == 0 || !align.is_power_of_two() {
        return None;
    }
    value.checked_add(align - 1).map(|v| v & !(align - 1))
}
