//! Перечисление MMIO-окон device-узлов FDT для cap-выдачи userspace.

use alloc::vec::Vec;

use drivers_common::DeviceNode;

use crate::{commons::is_compatible, fdt_adapter::FdtNode, tree_ext::NodeAddressExt};

/// MMIO-окно одного device-узла: физический базовый адрес и размер.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeviceWindow {
    pub base: usize,
    pub size: usize,
}

/// Перечисляет MMIO-окна прямых device-детей `root`, отбрасывая узлы с
/// `compatible` из `kernel_owned` (то, чем владеет само ядро - GIC, PL011).
/// Узлы без `reg` или `compatible` (memory, cpus, chosen и пр.) пропускаются.
/// Порядок соответствует порядку узлов в FDT и стабилен (индекс выдачи).
///
/// Перечисляются только прямые дети корня (плоская раскладка qemu virt); узлы
/// под промежуточной шиной с `ranges` не транслируются и не попадают в набор.
pub fn enumerate_device_windows(root: &FdtNode, kernel_owned: &[&str]) -> Vec<DeviceWindow> {
    let root_cells = root.cells_size().unwrap_or_default();
    let mut windows = Vec::new();

    for child in root.children() {
        if child.prop("reg").is_none() || child.prop("compatible").is_none() {
            continue;
        }
        if is_compatible(&child, kernel_owned) {
            continue;
        }
        for window in child.reg_iter(root_cells) {
            if window.size > 0 {
                windows.push(DeviceWindow {
                    base: window.offset,
                    size: window.size,
                });
            }
        }
    }

    windows
}
