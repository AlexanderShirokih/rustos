use fdt::{
    devicetree::{DeviceTree, Node},
    devicetreeext::{NodeExt, PropExt, translate_window},
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
    ancestors: Ancestors<'a>,
    interrupt_parent: Option<u32>,
}

#[derive(Clone, Copy)]
struct Ancestors<'a> {
    nodes: [Node<'a>; DESCRIPTION_DEPTH_MAX],
    len: usize,
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
            &mut Ancestors::new(root),
            root.interrupt_parent(),
            &mut search,
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
        let path = device.ancestors.path();
        let cells = path
            .last()
            .and_then(NodeExt::cells_size)
            .ok_or(ResolveError::Malformed)?;

        let windows = device
            .node
            .prop("reg")
            .and_then(|reg| reg.try_as_reg_list::<DEVICE_WINDOWS_MAX>(cells))
            .ok_or(ResolveError::NoMmio)?;
        let window = windows
            .get(usize::from(index))
            .ok_or(ResolveError::ResourceOutOfRange)?;
        let window = translate_window(path, *window).ok_or(ResolveError::Untranslatable)?;

        Ok(DeviceMmio {
            base: window.offset as u64,
            size: window.size as u64,
        })
    }
}

impl<'a> Ancestors<'a> {
    const fn new(root: Node<'a>) -> Self {
        Self {
            nodes: [root; DESCRIPTION_DEPTH_MAX],
            len: 1,
        }
    }

    fn path(&self) -> &[Node<'a>] {
        self.nodes.get(..self.len).unwrap_or_default()
    }

    fn push(&mut self, node: Node<'a>) -> Option<()> {
        *self.nodes.get_mut(self.len)? = node;
        self.len += 1;
        Some(())
    }

    fn pop(&mut self) {
        self.len -= 1;
    }
}

fn locate<'a>(
    ancestors: &mut Ancestors<'a>,
    inherited: Option<u32>,
    search: &mut Search<'_>,
) -> Option<Located<'a>> {
    let bus = *ancestors.path().last()?;

    for child in bus.children() {
        let interrupt_parent = child.interrupt_parent().or(inherited);

        if child.is_compatible(search.selector.compatible) {
            if search.seen == search.selector.instance {
                return Some(Located {
                    node: child,
                    ancestors: *ancestors,
                    interrupt_parent,
                });
            }
            search.seen += 1;
        }

        if ancestors.push(child).is_none() {
            continue;
        }

        let found = locate(ancestors, interrupt_parent, search);
        ancestors.pop();

        if found.is_some() {
            return found;
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
