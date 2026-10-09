use core::iter;

use fdt::{
    devicetree::{DeviceTree, Node},
    devicetreeext::{NodeExt, NodePath, PropExt, available_path, translate_window},
};
use platform_abi::DeviceId;

use crate::{
    binding::{ControllerBinding, ControllerBindings, InterruptSpecifier},
    platform::{DeviceInterrupt, DeviceMmio, PlatformDescription, ResolveError},
};

const CELL_SIZE: usize = size_of::<u32>();

const DESCRIPTION_DEPTH_MAX: usize = 16;

/// Описание платформы поверх device tree.
pub struct DeviceTreePlatform<'a> {
    tree: &'a DeviceTree<'a>,
    bindings: ControllerBindings,
}

struct Controller<'a> {
    binding: &'a ControllerBinding,
    cells: usize,
}

impl<'a> DeviceTreePlatform<'a> {
    /// Собирает описание платформы из дерева `tree` и толкует ячейки прерываний дерева по
    /// реестру `bindings`.
    #[must_use]
    pub const fn new(tree: &'a DeviceTree<'a>, bindings: ControllerBindings) -> Self {
        Self { tree, bindings }
    }

    fn root(&self) -> Result<Node<'a>, ResolveError> {
        self.tree.root().ok_or(ResolveError::Malformed)
    }

    fn device(&self, id: &DeviceId) -> Result<NodePath<'a>, ResolveError> {
        available_path(self.tree, id.as_str()).ok_or(ResolveError::DeviceNotFound)
    }

    fn controller(&self, path: &NodePath<'a>) -> Result<Controller<'_>, ResolveError> {
        let phandle = iter::once(path.node())
            .chain(path.ancestors().iter().rev().copied())
            .find_map(|node| node.interrupt_parent())
            .ok_or(ResolveError::Malformed)?;
        let node = by_phandle(self.root()?, phandle, DESCRIPTION_DEPTH_MAX)
            .ok_or(ResolveError::Malformed)?;
        let binding = node
            .prop("compatible")
            .and_then(PropExt::into_string_list_iter)
            .ok_or(ResolveError::UnknownController)?
            .find_map(|compatible| self.bindings.lookup(compatible))
            .ok_or(ResolveError::UnknownController)?;
        let cells = node.interrupt_cells().ok_or(ResolveError::Malformed)?;
        if cells == 0 {
            return Err(ResolveError::Malformed);
        }
        Ok(Controller { binding, cells })
    }
}

impl PlatformDescription for DeviceTreePlatform<'_> {
    fn interrupt(&self, device: &DeviceId, resource: u16) -> Result<DeviceInterrupt, ResolveError> {
        let path = self.device(device)?;
        let interrupts = path
            .node()
            .prop("interrupts")
            .ok_or(ResolveError::NoInterrupts)?;
        let controller = self.controller(&path)?;

        let stride = controller.cells * CELL_SIZE;
        let at = usize::from(resource) * stride;
        let cells = interrupts
            .value()
            .get(at..at + stride)
            .ok_or(ResolveError::ResourceOutOfRange)?;
        controller
            .binding
            .decode(InterruptSpecifier::new(cells))
            .ok_or(ResolveError::Malformed)
    }

    fn mmio(&self, device: &DeviceId, resource: u16) -> Result<DeviceMmio, ResolveError> {
        let path = self.device(device)?;
        let buses = path.ancestors();
        let cells = buses
            .last()
            .and_then(NodeExt::cells_size)
            .ok_or(ResolveError::Malformed)?;

        let reg = path
            .node()
            .prop("reg")
            .filter(|reg| !reg.value().is_empty())
            .ok_or(ResolveError::NoMmio)?;
        let window = reg
            .try_as_reg(cells, usize::from(resource))
            .ok_or(ResolveError::ResourceOutOfRange)?;
        let window = translate_window(buses, window).ok_or(ResolveError::Untranslatable)?;

        Ok(DeviceMmio {
            base: window.offset as u64,
            size: window.size as u64,
        })
    }
}

fn by_phandle(bus: Node<'_>, phandle: u32, depth: usize) -> Option<Node<'_>> {
    let depth = depth.checked_sub(1)?;
    for child in bus.children() {
        if child.phandle() == Some(phandle) {
            return Some(child);
        }
        if let Some(found) = by_phandle(child, phandle, depth) {
            return Some(found);
        }
    }
    None
}
