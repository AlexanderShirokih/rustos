//! Сверка узла FDT со списком строк совместимости и доступ к его MMIO-окнам из probe-функции.

use alloc::vec::Vec;

use drivers_common::{
    DeviceNode, NodeProperty,
    probe::{ProbeContext, ProbeError},
    services::mmio::MmioAddress,
};
use fdt::{
    devicetree::Node,
    devicetreeext::{self, translate_window},
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
