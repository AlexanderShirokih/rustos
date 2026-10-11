use alloc::vec::Vec;

use platform_abi::{Description, DeviceId};

const LINES: &str = "gpios";
const LINES_SUFFIX: &str = "-gpios";
const LINE_CELLS: &str = "#gpio-cells";

/// Запись линии в свойстве `gpios` либо `<name>-gpios`.
pub struct LineRecord {
    /// Путь контроллера записи.
    pub controller: DeviceId,
    /// Ячейки записи после ссылки на контроллер, число которых задаёт `#gpio-cells`
    /// контроллера.
    pub cells: Vec<u32>,
}

/// Возвращает запись `index` свойства `property` доступного узла `node`.
///
/// Возвращает `None`, если:
/// - узла нет в описании либо он недоступен;
/// - имя свойства не `gpios` и не оканчивается на `-gpios`, у узла нет свойства либо записи
///   `index`;
/// - ссылка записи до `index` либо в ней не называет узла, у узла нет `#gpio-cells` либо
///   запись короче его ячеек.
pub fn line(
    description: &Description<'_>,
    node: &DeviceId,
    property: &str,
    index: u8,
) -> Option<LineRecord> {
    if property != LINES && !property.ends_with(LINES_SUFFIX) {
        return None;
    }

    let specifier = description
        .node(node)?
        .specifiers(property, LINE_CELLS)?
        .nth(usize::from(index))?
        .ok()?;

    Some(LineRecord {
        cells: specifier.cells().collect(),
        controller: specifier.node,
    })
}
