//! Общие утилиты для драйверов.

use drivers_common::ProbeContext;
use fdt::devicetreeext::{NodeExt, PropExt};

/// Расширение ProbeContext для работы с адресами регистров.
pub(crate) trait ProbeContextExt {
    fn reg_offset<const N: usize>(&self, reg_index: usize) -> usize;
}

impl ProbeContextExt for ProbeContext<'_> {
    fn reg_offset<const N: usize>(&self, reg_index: usize) -> usize {
        let node = self.node();
        let bus = self.parent(node);
        let root = bus.and_then(|parent| self.parent(parent));

        let parent_cell_size = bus
            .and_then(|parent| parent.cells_size())
            .unwrap_or_default();
        let root_cell_size = root.and_then(|root| root.cells_size()).unwrap_or_default();

        let bus_range = bus
            .and_then(|parent| parent.try_get_range(root_cell_size))
            .unwrap_or_default();

        let address_space = node
            .prop("reg")
            .and_then(|prop| prop.try_as_reg_list::<N>(parent_cell_size))
            .and_then(|reg_list| reg_list.get(reg_index).copied());

        let offset = address_space
            .map(|address_space| address_space.offset)
            .unwrap_or_default();

        bus_range.parent_addr() + offset - bus_range.child_addr()
    }
}
