use fdt::{
    devicetree::{DeviceTree, Node},
    devicetreeext::{NodeExt, PropExt},
};

use crate::{
    binding::{ControllerBinding, ControllerBindings, InterruptSpecifier},
    platform::{DeviceInterrupt, DeviceMmio, DeviceSelector, PlatformDescription, ResolveError},
};

const CELL_SIZE: usize = size_of::<u32>();

const DEVICE_WINDOWS_MAX: usize = 4;

const DESCRIPTION_DEPTH_MAX: usize = 16;

/// Описание платформы поверх device tree.
pub struct DeviceTreePlatform<'a> {
    tree: &'a DeviceTree<'a>,
    bindings: ControllerBindings,
}

struct Located<'a> {
    node: Node<'a>,
    bus: Node<'a>,
    interrupt_parent: Option<u32>,
    translated_bus: bool,
}

struct Controller<'a> {
    binding: &'a ControllerBinding,
    cells: usize,
}

struct Search<'a> {
    selector: DeviceSelector<'a>,
    seen: u16,
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

    fn device(&self, selector: DeviceSelector<'_>) -> Result<Located<'a>, ResolveError> {
        let root = self.root()?;
        let mut search = Search { selector, seen: 0 };
        locate(
            root,
            root.interrupt_parent(),
            false,
            &mut search,
            DESCRIPTION_DEPTH_MAX,
        )
        .ok_or(ResolveError::DeviceNotFound)
    }

    fn controller(&self, device: &Located<'a>) -> Result<Controller<'_>, ResolveError> {
        let phandle = device.interrupt_parent.ok_or(ResolveError::Malformed)?;
        let node = by_phandle(self.root()?, phandle, DESCRIPTION_DEPTH_MAX)
            .ok_or(ResolveError::Malformed)?;
        let binding = node
            .prop("compatible")
            .ok_or(ResolveError::UnknownController)?
            .into_string_list_iter()
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
    fn interrupt(
        &self,
        device: DeviceSelector<'_>,
        index: u16,
    ) -> Result<DeviceInterrupt, ResolveError> {
        let device = self.device(device)?;
        let interrupts = device
            .node
            .prop("interrupts")
            .ok_or(ResolveError::NoInterrupts)?;
        let controller = self.controller(&device)?;

        let stride = controller.cells * CELL_SIZE;
        let at = usize::from(index) * stride;
        let cells = interrupts
            .value()
            .get(at..at + stride)
            .ok_or(ResolveError::ResourceOutOfRange)?;
        controller
            .binding
            .decode(InterruptSpecifier::new(cells))
            .ok_or(ResolveError::Malformed)
    }

    fn mmio(&self, device: DeviceSelector<'_>, index: u16) -> Result<DeviceMmio, ResolveError> {
        let device = self.device(device)?;
        if device.translated_bus {
            return Err(ResolveError::BusTranslation);
        }
        let cells = device.bus.cells_size().ok_or(ResolveError::Malformed)?;
        let windows = device
            .node
            .prop("reg")
            .and_then(|reg| reg.try_as_reg_list::<DEVICE_WINDOWS_MAX>(cells))
            .ok_or(ResolveError::NoMmio)?;
        let window = windows
            .get(usize::from(index))
            .ok_or(ResolveError::ResourceOutOfRange)?;
        Ok(DeviceMmio {
            base: window.offset as u64,
            size: window.size as u64,
        })
    }
}

fn locate<'a>(
    bus: Node<'a>,
    inherited: Option<u32>,
    translated_bus: bool,
    search: &mut Search<'_>,
    depth: usize,
) -> Option<Located<'a>> {
    let depth = depth.checked_sub(1)?;
    for child in bus.children() {
        let interrupt_parent = child.interrupt_parent().or(inherited);
        if child.is_compatible(search.selector.compatible) {
            if search.seen == search.selector.instance {
                return Some(Located {
                    node: child,
                    bus,
                    interrupt_parent,
                    translated_bus,
                });
            }
            search.seen += 1;
        }
        // Окна потомков лежат в координатах шины, и без `ranges` шина их в
        // родительское пространство не отображает.
        let identity = child
            .prop("ranges")
            .is_some_and(|ranges| ranges.value().is_empty());
        if let Some(found) = locate(
            child,
            interrupt_parent,
            translated_bus || !identity,
            search,
            depth,
        ) {
            return Some(found);
        }
    }
    None
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
