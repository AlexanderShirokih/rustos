//! Общие утилиты для драйверов.

use drivers_common::{DeviceNode, NodeProperty, ProbeContext};
use crate::tree_ext::NodeAddressExt;
use crate::fdt_adapter::FdtNode;

pub(crate) type FdtProbeContext<'a> = ProbeContext<FdtNode<'a>>;

/// Расширение ProbeContext для работы с адресами регистров.
pub(crate) trait ProbeContextExt {
    fn reg_offset<const N: usize>(&self, reg_index: usize) -> usize;
}

impl ProbeContextExt for FdtProbeContext<'_> {
    fn reg_offset<const N: usize>(&self, reg_index: usize) -> usize {
        let node = self.node();
        let bus = self.parent(node);
        let root = bus.and_then(|parent| self.parent(parent));

        let parent_cell_size = bus
            .and_then(|parent| parent.cells_size())
            .unwrap_or_default();
        let root_cell_size = root.and_then(|root| root.cells_size()).unwrap_or_default();

        let bus_range = bus
            .and_then(|parent| parent.range_to_parent(root_cell_size))
            .unwrap_or_default();

        let address_space = node
            .reg_list(parent_cell_size, N)
            .get(reg_index)
            .copied();

        let offset = address_space
            .map(|address_space| address_space.offset)
            .unwrap_or_default();

        bus_range.parent + offset - bus_range.child
    }
}

/// Проверяет, содержит ли узел хотя бы одну из указанных строк совместимости.
pub(crate) fn is_compatible(node: &FdtNode, candidates: &[&str]) -> bool {
    let Some(prop) = node.prop("compatible") else {
        return false;
    };
    let data = prop.raw();
    candidates.iter().any(|candidate| {
        data.split(|&b| b == 0)
            .filter(|s| !s.is_empty())
            .any(|bytes| core::str::from_utf8(bytes).ok() == Some(candidate))
    })
}

/// Возвращает ошибку, если узел не содержит ни одной из указанных строк совместимости.
pub(crate) fn require_compatible(
    node: &FdtNode,
    candidates: &[&str],
) -> drivers_common::ProbeResult<()> {
    if is_compatible(node, candidates) {
        Ok(())
    } else {
        Err(drivers_common::ProbeError::Unsupported("not compatible"))
    }
}
