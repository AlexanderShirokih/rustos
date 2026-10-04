//! Сверка узла FDT со списком строк совместимости и доступ к его MMIO-окнам из probe-функции.

use alloc::vec::Vec;

use drivers_common::{
    probe::{ProbeContext, ProbeError},
    services::mmio::MmioAddress,
};
use fdt::{
    devicetree::Node,
    devicetreeext::{self, PropExt, translate_window},
};

use crate::{
    fdt_adapter::FdtNode,
    tree_ext::{AddressSpace, NodeAddressExt},
};

pub type FdtProbeContext<'a> = ProbeContext<FdtNode<'a>>;

/// Добавляет `ProbeContext`-у чтение `reg`-записи узла в адресном пространстве корня.
pub trait ProbeContextExt {
    /// Запись `reg_index` свойства `reg` узла, переведённая через `ranges` шин на пути к корню.
    /// `None`, если записи нет либо адрес не транслируется.
    fn get_address(&self, reg_index: usize) -> Option<AddressSpace>;

    fn get_mmio_address(&self, reg_index: usize) -> Option<MmioAddress> {
        self.get_address(reg_index)?.as_mmio()
    }
}

impl ProbeContextExt for FdtProbeContext<'_> {
    fn get_address(&self, reg_index: usize) -> Option<AddressSpace> {
        let (node, buses) = self.hierarchy().split_last()?;
        let cells = buses.last()?.cells_size().unwrap_or_default();
        let window = node.reg_iter(cells).nth(reg_index)?;
        let path: Vec<Node<'_>> = buses.iter().map(FdtNode::inner).collect();

        let translated = translate_window(
            &path,
            devicetreeext::AddressSpace {
                offset: window.offset,
                size: window.size,
            },
        )?;

        Some(AddressSpace {
            offset: translated.offset,
            size: translated.size,
        })
    }
}

/// Проверяет, содержит ли узел хотя бы одну из указанных строк совместимости.
pub fn is_compatible(node: &FdtNode, candidates: &[&str]) -> bool {
    node.inner()
        .prop("compatible")
        .and_then(PropExt::into_string_list_iter)
        .is_some_and(|mut compatible| compatible.any(|string| candidates.contains(&string)))
}

/// Возвращает ошибку, если узел не содержит ни одной из указанных строк совместимости.
pub fn require_compatible(node: &FdtNode, candidates: &[&str]) -> Result<(), ProbeError> {
    if is_compatible(node, candidates) {
        Ok(())
    } else {
        Err(ProbeError::Unsupported("not compatible"))
    }
}
