use alloc::{vec, vec::Vec};

use platform_abi::Node;

use crate::error::PinsError;

const PINS: &str = "pins";
const GPIO_PREFIX: &str = "gpio";

/// Возвращает наборы узла состояния `state`. Узел с `pins` - набор, у узла без `pins` наборами
/// служат его доступные потомки с `pins`.
pub fn sets(state: Node<'_>) -> Vec<Node<'_>> {
    if state.bytes(PINS).is_some() {
        return vec![state];
    }
    state
        .children()
        .filter(|child| child.bytes(PINS).is_some())
        .collect()
}

/// Проверяет, что узел состояния без `pins` не задаёт своих свойств: наборы такого состояния -
/// его потомки.
///
/// Отказы:
/// - [`PinsError::Unsupported`] - у узла без `pins` есть свойство.
pub fn check_state(state: &Node<'_>) -> Result<(), PinsError> {
    if state.bytes(PINS).is_none() && state.property_names().next().is_some() {
        return Err(PinsError::Unsupported);
    }
    Ok(())
}

/// Имена выводов набора `set` из `pins` либо `None`, если свойства нет или его значение
/// повреждено.
pub fn pin_names<'a>(set: &Node<'a>) -> Option<impl Iterator<Item = &'a str>> {
    set.strings(PINS).ok().flatten()
}

/// Номер вывода `N` из имени `gpioN`.
pub fn pin_number(name: &str) -> Option<u16> {
    let digits = name.strip_prefix(GPIO_PREFIX)?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}
