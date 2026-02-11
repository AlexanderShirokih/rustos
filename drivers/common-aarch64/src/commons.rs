//! Общие утилиты для драйверов.

use crate::fdt_adapter::FdtNode;
use crate::tree_ext::{AddressSpace, NodeAddressExt};
use drivers_common::{DeviceNode, MmioAddress, NodeProperty, ProbeContext, ProbeError};

pub type FdtProbeContext<'a> = ProbeContext<FdtNode<'a>>;

/// Расширение ProbeContext для работы с адресами регистров.
pub trait ProbeContextExt {
    fn reg_address(&self, reg_index: usize) -> AddressSpace;

    fn reg_mmio_address(&self, reg_index: usize) -> Option<MmioAddress> {
        self.reg_address(reg_index).as_mmio()
    }
}

impl<N> ProbeContextExt for ProbeContext<N>
where
    N: NodeAddressExt,
{
    fn reg_address(&self, reg_index: usize) -> AddressSpace {
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
            .reg_iter(parent_cell_size)
            .skip(reg_index)
            .next()
            .unwrap_or(AddressSpace { offset: 0, size: 0 });

        AddressSpace {
            offset: bus_range.parent + address_space.offset - bus_range.child,
            size: address_space.size,
        }
    }
}

/// Проверяет, содержит ли узел хотя бы одну из указанных строк совместимости.
pub fn is_compatible(node: &FdtNode, candidates: &[&str]) -> bool {
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
pub fn require_compatible(node: &FdtNode, candidates: &[&str]) -> Result<(), ProbeError> {
    if is_compatible(node, candidates) {
        Ok(())
    } else {
        Err(ProbeError::Unsupported("not compatible"))
    }
}
