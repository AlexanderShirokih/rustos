//! Перечисление MMIO-окон device-узлов FDT для выдачи userspace-драйверам.

use alloc::{collections::BTreeSet, vec::Vec};

use fdt::devicetreeext::{DeviceWindow, device_windows};

use crate::fdt_adapter::FdtNode;

/// Перечисляет MMIO-окна устройств дерева `root` в адресном пространстве корня по правилам
/// [`device_windows`]. Узел, чей ключ входит в `claimed` (ядро подобрало под него драйвер
/// при probe), и его поддерево дают окна с пометкой [`DeviceWindow::excluded`].
/// Окна идут в порядке узлов FDT и их `reg`-записей.
pub fn enumerate_device_windows(root: &FdtNode, claimed: &BTreeSet<usize>) -> Vec<DeviceWindow> {
    device_windows(root.inner(), |node| claimed.contains(&node.key())).collect()
}
