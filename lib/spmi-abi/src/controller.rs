use platform_abi::{Description, DeviceId, Node};

const ADDRESS_CELLS: u32 = 2;
const SIZE_CELLS: u32 = 0;

/// Узел контроллера SPMI устройства `device`: ближайшего предка, у которого явно заданы
/// `#address-cells = <2>` и `#size-cells = <0>`, как биндинг `spmi.yaml` описывает контроллер
/// SPMI. Значения ячеек по умолчанию не применяются. Узел, которого нет в описании, и узел без
/// такого предка контроллера не имеют.
#[must_use]
pub fn controller<'d>(description: &'d Description<'_>, device: &DeviceId) -> Option<Node<'d>> {
    description
        .ancestors(device)
        .map(|ancestor| ancestor.node)
        .find(|node| {
            node.u32("#address-cells") == Ok(Some(ADDRESS_CELLS))
                && node.u32("#size-cells") == Ok(Some(SIZE_CELLS))
        })
}
