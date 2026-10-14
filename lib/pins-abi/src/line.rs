use alloc::{format, string::String, vec::Vec};

use platform_abi::{Description, DeviceId};

const LINES: &str = "gpios";
const LINES_SUFFIX: &str = "-gpios";
const LINE_CELLS: &str = "#gpio-cells";

const INTERRUPTS: &str = "interrupts-extended";
const INTERRUPT_CELLS: &str = "#interrupt-cells";

/// Флаг записи линии с низким активным уровнем.
pub const GPIO_ACTIVE_LOW: u32 = 1 << 0;

/// Запись линии в свойстве `gpios`, `<name>-gpios` либо `interrupts-extended`.
pub struct LineRecord {
    /// Путь контроллера записи.
    pub controller: DeviceId,
    /// Ячейки записи после ссылки на контроллер, число которых задаёт `#gpio-cells` либо
    /// `#interrupt-cells` контроллера.
    pub cells: Vec<u32>,
}

/// Возвращает запись `index` линии `name` доступного узла `node` сессии узла `session`.
/// Линию с именем описывает свойство `<name>-gpios`, линию с пустым именем - свойство `gpios`.
///
/// Возвращает `None`, если:
/// - `node` не узел `session` и не его потомок;
/// - узла нет в описании либо он недоступен;
/// - у узла нет свойства линии либо записи `index`;
/// - ссылка записи до `index` либо в ней не называет узла, у узла нет `#gpio-cells` либо
///   запись короче его ячеек.
pub fn line(
    description: &Description<'_>,
    session: &DeviceId,
    node: &DeviceId,
    name: &str,
    index: u8,
) -> Option<LineRecord> {
    let property = if name.is_empty() {
        String::from(LINES)
    } else {
        format!("{name}{LINES_SUFFIX}")
    };
    record(description, session, node, &property, LINE_CELLS, index)
}

/// Возвращает запись `index` свойства `interrupts-extended` доступного узла `node` сессии узла
/// `session`.
///
/// Возвращает `None`, если:
/// - `node` не узел `session` и не его потомок;
/// - узла нет в описании либо он недоступен;
/// - у узла нет свойства либо записи `index`;
/// - ссылка записи до `index` либо в ней не называет узла, у узла нет `#interrupt-cells` либо
///   запись короче его ячеек.
pub fn interrupt(
    description: &Description<'_>,
    session: &DeviceId,
    node: &DeviceId,
    index: u8,
) -> Option<LineRecord> {
    record(
        description,
        session,
        node,
        INTERRUPTS,
        INTERRUPT_CELLS,
        index,
    )
}

fn record(
    description: &Description<'_>,
    session: &DeviceId,
    node: &DeviceId,
    property: &str,
    cells: &'static str,
    index: u8,
) -> Option<LineRecord> {
    let inside = node == session
        || node
            .as_str()
            .strip_prefix(session.as_str())
            .is_some_and(|rest| rest.starts_with('/'));
    if !inside {
        return None;
    }

    let specifier = description
        .node(node)?
        .specifiers(property, cells)?
        .nth(usize::from(index))?
        .ok()?;

    Some(LineRecord {
        cells: specifier.cells().collect(),
        controller: specifier.node,
    })
}
