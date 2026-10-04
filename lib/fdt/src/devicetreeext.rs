use core::{
    cmp::{max, min},
    num::NonZeroUsize,
    str::Split,
};

use collections::Vec;

use crate::devicetree::{DeviceTree, Node, NodeIter, Property};

const RANGES_FIELD_MAX_CELLS: usize = 2;

/// Предельная длина пути от корня до узла в узлах, считая корень и сам узел.
pub const BUS_PATH_MAX_NODES: usize = 16;

/// Содержит пару адрес (сдвиг) и размер
#[derive(Debug, Copy, Clone)]
pub struct AddressSpace {
    pub offset: usize,
    pub size: usize,
}

impl AddressSpace {
    pub fn start(&self) -> usize {
        self.offset
    }

    pub fn end(&self) -> usize {
        self.offset + self.size
    }
}

/// Кратность регистров устройства в 32-битных словах
#[derive(Debug, Copy, Clone)]
pub struct CellsSize {
    address_cells: usize,

    size_cells: usize,
}

impl CellsSize {
    pub fn stride(&self) -> usize {
        self.address_cells + self.size_cells
    }
}

impl Default for CellsSize {
    fn default() -> Self {
        CellsSize {
            address_cells: 2,
            size_cells: 1,
        }
    }
}

pub trait NodeExt {
    /// Узел доступен, если у него нет свойства `status` либо его значение `okay` или `ok`.
    fn is_available(&self) -> bool;

    /// Возвращает размерность ячеек адреса и размера для этого узла.
    fn cells_size(&self) -> Option<CellsSize>;

    /// База первого `reg`-окна узла в координатах шины `bus` (узел - её прямой
    /// потомок). Трансляция адресов промежуточных шин (`ranges`) не выполняется.
    fn first_reg_base(&self, bus: &Node<'_>) -> Option<u64>;

    /// Значение свойства `phandle`: идентификатор, которым на узел ссылаются
    /// другие узлы.
    fn phandle(&self) -> Option<u32>;

    /// Phandle контроллера прерываний узла из свойства `interrupt-parent`.
    fn interrupt_parent(&self) -> Option<u32>;

    /// Число ячеек одного interrupt-specifier из свойства `#interrupt-cells`.
    fn interrupt_cells(&self) -> Option<usize>;
}

impl NodeExt for Node<'_> {
    fn is_available(&self) -> bool {
        self.prop("status")
            .is_none_or(|prop| matches!(prop.as_cstr(), Some("okay" | "ok")))
    }

    fn cells_size(&self) -> Option<CellsSize> {
        let address_cells = self
            .prop("#address-cells")
            .map_or(2, |prop| prop.as_usize());

        let size_cells = self.prop("#size-cells").map_or(1, |prop| prop.as_usize());

        Some(CellsSize {
            address_cells,
            size_cells,
        })
    }

    fn first_reg_base(&self, bus: &Node<'_>) -> Option<u64> {
        let cells = bus.cells_size().unwrap_or_default();
        let windows = self.prop("reg")?.try_as_reg_list::<1>(cells)?;
        windows.get(0).map(|window| window.offset as u64)
    }

    fn phandle(&self) -> Option<u32> {
        self.prop("phandle")?.try_as_u32(0)
    }

    fn interrupt_parent(&self) -> Option<u32> {
        self.prop("interrupt-parent")?.try_as_u32(0)
    }

    fn interrupt_cells(&self) -> Option<usize> {
        let cells = self.prop("#interrupt-cells")?.try_as_u32(0)?;
        Some(cells as usize)
    }
}

/// Итератор окон устройств дерева, который строит [`device_windows`].
pub struct DeviceWindows<'a, F> {
    path: [Node<'a>; BUS_PATH_MAX_NODES],
    path_excluded: [bool; BUS_PATH_MAX_NODES],
    children: [Option<NodeIter<'a>>; BUS_PATH_MAX_NODES],
    depth: usize,
    device: Option<Node<'a>>,
    device_excluded: bool,
    reg_index: usize,
    excluded: F,
}

/// Окно устройства, которое выдаёт [`device_windows`].
#[derive(Debug, Copy, Clone)]
pub struct DeviceWindow {
    pub window: AddressSpace,
    /// Предикат `excluded` из [`device_windows`] вернул `true` для узла окна либо его предка.
    pub excluded: bool,
}

/// Причина, по которой [`page_regions`] не включает окно в регионы.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum SkippedWindow {
    /// Страницы окна пересекают страницы окна исключённого узла.
    SharesExcludedPage,
    /// Страницы окна недопустимы по [`is_window_admissible`] либо выходят за разрядность
    /// `usize`.
    Inadmissible,
}

/// Узел, который нашёл [`node_path`], и его предки от корня дерева.
#[derive(Clone, Copy)]
pub struct NodePath<'a> {
    node: Node<'a>,
    ancestors: [Node<'a>; BUS_PATH_MAX_NODES - 1],
    len: usize,
}

impl<'a> NodePath<'a> {
    /// Найденный узел.
    pub fn node(&self) -> Node<'a> {
        self.node
    }

    /// Предки найденного узла от корня, последним - его родитель.
    pub fn ancestors(&self) -> &[Node<'a>] {
        self.ancestors.get(..self.len).unwrap_or_default()
    }
}

/// Находит узел по пути `path` и возвращает его вместе с предками.
///
/// Путь начинается с `/`, его компоненты сравниваются с именами узлов целиком, алиасы не
/// разрешаются. Возвращает `None`, если в дереве нет узла и если путь длиннее
/// [`BUS_PATH_MAX_NODES`] узлов.
pub fn node_path<'a>(dt: &'a DeviceTree<'a>, path: &str) -> Option<NodePath<'a>> {
    let root = dt.root()?;
    let mut found = NodePath {
        node: root,
        ancestors: [root; BUS_PATH_MAX_NODES - 1],
        len: 0,
    };

    let names = path.strip_prefix('/')?;
    if names.is_empty() {
        return Some(found);
    }

    for name in names.split('/') {
        let child = found.node.children().find(|child| child.name() == name)?;
        *found.ancestors.get_mut(found.len)? = found.node;
        found.len += 1;
        found.node = child;
    }

    Some(found)
}

/// Как [`node_path`], но возвращает `None` и тогда, когда узел либо его предок недоступен по
/// [`NodeExt::is_available`].
pub fn available_path<'a>(dt: &'a DeviceTree<'a>, path: &str) -> Option<NodePath<'a>> {
    node_path(dt, path).filter(|found| {
        found.node().is_available() && found.ancestors().iter().all(NodeExt::is_available)
    })
}

/// Статический reg-диапазон дочернего узла `/reserved-memory`.
#[derive(Debug, Copy, Clone)]
pub struct ReservedRange {
    pub range: AddressSpace,
    /// Узел содержит свойство `no-map`: ОС не отображает диапазон.
    pub no_map: bool,
}

/// Непустые reg-диапазоны узлов `memory` корня дерева, то есть RAM, которую описывает дерево.
///
/// Узел относится к RAM по `device_type = "memory"`, а без `device_type` - по имени,
/// начинающемуся с `memory`.
pub fn find_ram_regions<'a>(
    dt: &'a DeviceTree<'a>,
) -> Option<impl Iterator<Item = AddressSpace> + 'a> {
    let root = dt.root()?;
    let cells_size = root.cells_size().unwrap_or_default();

    Some(
        root.children()
            .filter(|node| {
                node.prop("device_type")
                    .and_then(|device_type| {
                        device_type.as_cstr().map(|str| str.starts_with("memory"))
                    })
                    .unwrap_or_else(|| node.name().starts_with("memory"))
            })
            .filter_map(|node| node.prop("reg"))
            .flat_map(move |prop| {
                prop.try_as_reg_list::<8>(cells_size)
                    .unwrap_or_default()
                    .into_iter()
            })
            .filter(|offset_size| offset_size.size > 0),
    )
}

/// Статические reg-диапазоны дочерних узлов `/reserved-memory`.
pub fn reserved_memory_ranges<'a>(
    dt: &'a DeviceTree<'a>,
) -> Option<impl Iterator<Item = ReservedRange> + 'a> {
    let reserved = dt.find("/reserved-memory")?;
    let cells_size = reserved.cells_size().unwrap_or_default();

    Some(
        reserved
            .children()
            .filter_map(move |node| {
                let no_map = node.prop("no-map").is_some();
                let ranges = node.prop("reg")?.try_as_reg_list::<8>(cells_size)?;
                Some(
                    ranges
                        .into_iter()
                        .map(move |range| ReservedRange { range, no_map }),
                )
            })
            .flatten()
            .filter(|reserved| reserved.range.size > 0),
    )
}

/// Проверяет, допустимо ли окно device-памяти `window` относительно RAM дерева.
///
/// Окно, которое не пересекает RAM из [`find_ram_regions`], допустимо. Окно, пересекающее
/// RAM, допустимо, только если целиком лежит в одном диапазоне `/reserved-memory` с
/// `no-map`. Окно, конец которого выходит за разрядность `usize`, недопустимо.
pub fn is_window_admissible(dt: &DeviceTree<'_>, window: AddressSpace) -> bool {
    let Some(end) = window.offset.checked_add(window.size) else {
        return false;
    };

    let overlaps_ram = find_ram_regions(dt)
        .into_iter()
        .flatten()
        .any(|ram| ram.start() < end && window.offset < ram.end());

    !overlaps_ram
        || reserved_memory_ranges(dt)
            .into_iter()
            .flatten()
            .filter(|reserved| reserved.no_map)
            .any(|reserved| reserved.range.start() <= window.offset && end <= reserved.range.end())
}

impl<'a, F> DeviceWindows<'a, F> {
    fn next_window(&mut self, device: &Node<'a>) -> Option<AddressSpace> {
        device.prop("compatible")?;
        let reg = device.prop("reg")?;
        let path = self.path.get(..self.depth)?;
        let cells = path.last()?.cells_size().unwrap_or_default();

        while self.reg_index < reg_entry_count(&reg, cells) {
            let window = reg_entry(&reg, cells, self.reg_index);
            self.reg_index += 1;

            let translated = window
                .filter(|window| window.size > 0)
                .and_then(|window| translate_window(path, window));
            if translated.is_some() {
                return translated;
            }
        }

        None
    }

    fn descend(&mut self, node: Node<'a>) {
        let reserved_memory = self.depth == 1 && node.name() == "reserved-memory";
        if node.prop("ranges").is_none() || reserved_memory {
            return;
        }

        if let (Some(bus), Some(bus_excluded), Some(children)) = (
            self.path.get_mut(self.depth),
            self.path_excluded.get_mut(self.depth),
            self.children.get_mut(self.depth),
        ) {
            *bus = node;
            *bus_excluded = self.device_excluded;
            *children = Some(node.children());
            self.depth += 1;
        }
    }

    fn ascend(&mut self) {
        self.depth -= 1;
        if let Some(children) = self.children.get_mut(self.depth) {
            *children = None;
        }
    }
}

impl<'a, F> Iterator for DeviceWindows<'a, F>
where
    F: FnMut(&Node<'a>) -> bool,
{
    type Item = DeviceWindow;

    fn next(&mut self) -> Option<DeviceWindow> {
        loop {
            if let Some(device) = self.device {
                if let Some(window) = self.next_window(&device) {
                    return Some(DeviceWindow {
                        window,
                        excluded: self.device_excluded,
                    });
                }
                self.device = None;
                self.descend(device);
            }

            let level = self.depth.checked_sub(1)?;
            let children = self.children.get_mut(level)?.as_mut()?;
            match children.next() {
                Some(node) => {
                    let bus_excluded = self.path_excluded.get(level).copied().unwrap_or_default();
                    self.device_excluded = bus_excluded || (self.excluded)(&node);
                    self.device = Some(node);
                    self.reg_index = 0;
                }
                None => self.ascend(),
            }
        }
    }
}

pub trait PropExt<'a> {
    /// Извлекает список регистров (адрес + размер) из свойства `reg`.
    fn try_as_reg_list<const N: usize>(
        &self,
        cells_size: CellsSize,
    ) -> Option<Vec<AddressSpace, N>>;

    /// Возвращает строки значения (например, `compatible`). Значение без завершающего нуля и не
    /// в UTF-8 даёт `None`.
    fn into_string_list_iter(self) -> Option<StringList<'a>>;
}

impl Property<'_> {
    fn try_read_cell(&self, cells: usize, offset: usize) -> Option<u64> {
        let mut value = 0u64;

        for cell in 0..cells {
            let reg = u64::from(self.try_as_u32((offset + cell) * size_of::<u32>())?);
            value = (value << 32) | reg;
        }

        Some(value)
    }
}

impl<'a> PropExt<'a> for Property<'a> {
    fn try_as_reg_list<const N: usize>(
        &self,
        cells_size: CellsSize,
    ) -> Option<Vec<AddressSpace, N>> {
        let count = reg_entry_count(self, cells_size);
        if count == 0 {
            return None;
        }

        let mut result = Vec::new();

        for index in 0..min(N, count) {
            result.push(reg_entry(self, cells_size, index)?);
        }

        Some(result)
    }

    fn into_string_list_iter(self) -> Option<StringList<'a>> {
        let text = str::from_utf8(self.value().strip_suffix(&[0])?).ok()?;
        Some(StringList {
            strings: text.split('\0'),
        })
    }
}

/// Непустые строки значения свойства в порядке значения. Список пропускает пустые строки между
/// нулями, как сравнение `compatible` в Linux.
#[derive(Clone)]
pub struct StringList<'a> {
    strings: Split<'a, char>,
}

impl<'a> Iterator for StringList<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        self.strings.find(|string| !string.is_empty())
    }
}

/// Перечисляет окна `reg` узлов с `compatible` в адресном пространстве корня `root`.
///
/// Обход спускается только в узлы со свойством `ranges` и не спускается в `/reserved-memory`.
/// Узел, для которого `excluded` возвращает `true`, и его поддерево дают окна с пометкой
/// [`DeviceWindow::excluded`]. Окно нулевого размера и окно, которое [`translate_window`] не
/// переводит, пропускаются. Устройство за путём длиннее [`BUS_PATH_MAX_NODES`] узлов не даёт
/// окон.
pub fn device_windows<'a, F>(root: Node<'a>, excluded: F) -> DeviceWindows<'a, F>
where
    F: FnMut(&Node<'a>) -> bool,
{
    let mut children = [const { None }; BUS_PATH_MAX_NODES];
    if let Some(first) = children.first_mut() {
        *first = Some(root.children());
    }

    DeviceWindows {
        path: [root; BUS_PATH_MAX_NODES],
        path_excluded: [false; BUS_PATH_MAX_NODES],
        children,
        depth: 1,
        device: None,
        device_excluded: false,
        reg_index: 0,
        excluded,
    }
}

/// Строит из окон `windows` регионы из целых страниц `page_size` по возрастанию базы и
/// перезаписывает элементы `windows`. Окна с общей страницей входят в один регион. Окна
/// исключённых узлов в регионы не входят, а окно, отвергнутое по странице такого узла либо по
/// [`is_window_admissible`], передаётся в `skipped`.
pub fn page_regions<'w>(
    dt: &DeviceTree<'_>,
    windows: &'w mut [DeviceWindow],
    page_size: NonZeroUsize,
    mut skipped: impl FnMut(AddressSpace, SkippedWindow),
) -> impl Iterator<Item = AddressSpace> + 'w {
    windows.sort_unstable_by_key(|window| (!window.excluded, window.window.offset));
    let (excluded, free) = windows.split_at_mut(windows.partition_point(|window| window.excluded));

    let mut merged: usize = 0;
    for index in 0..free.len() {
        let Some(window) = free.get(index).map(|window| window.window) else {
            break;
        };
        let pages = match admitted_pages(dt, excluded, window, page_size) {
            Ok(pages) => pages,
            Err(reason) => {
                skipped(window, reason);
                continue;
            }
        };
        match merged.checked_sub(1).and_then(|last| free.get_mut(last)) {
            Some(last) if pages.start() < last.window.end() => {
                last.window.size = max(last.window.end(), pages.end()) - last.window.start();
            }
            _ => {
                if let Some(slot) = free.get_mut(merged) {
                    slot.window = pages;
                }
                merged += 1;
            }
        }
    }

    let free: &'w [DeviceWindow] = free;
    free.iter().take(merged).map(|window| window.window)
}

/// Переводит окно `window` в адресное пространство корня дерева.
///
/// `path` перечисляет узлы от корня до шины, в координатах которой задано окно. Шина с
/// пустым `ranges` сохраняет адрес окна. Запись `ranges` переносит окно, которое целиком
/// лежит в этой записи.
///
/// Возвращает `None`, если на пути есть шина без `ranges`, если окно не укладывается ни в
/// одну запись, если адрес либо размер записи занимает больше двух ячеек и если конец окна
/// выходит за разрядность `usize`.
pub fn translate_window(path: &[Node<'_>], window: AddressSpace) -> Option<AddressSpace> {
    path.windows(2)
        .rev()
        .try_fold(window, |window, pair| match pair {
            [parent, bus] => translate_through(bus, parent, window),
            _ => None,
        })
}

fn translate_through(
    bus: &Node<'_>,
    parent: &Node<'_>,
    window: AddressSpace,
) -> Option<AddressSpace> {
    let ranges = bus.prop("ranges")?;

    if ranges.value().is_empty() {
        return Some(window);
    }

    let child = bus.cells_size().unwrap_or_default();
    let parent_address_cells = parent.cells_size().unwrap_or_default().address_cells;

    if child.address_cells > RANGES_FIELD_MAX_CELLS
        || child.size_cells > RANGES_FIELD_MAX_CELLS
        || parent_address_cells > RANGES_FIELD_MAX_CELLS
    {
        return None;
    }

    let stride = child.stride() + parent_address_cells;
    let entries = (ranges.value().len() / size_of::<u32>()).checked_div(stride)?;

    (0..entries).find_map(|entry| {
        let at = entry * stride;
        let child_base = ranges.try_read_cell(child.address_cells, at)?;
        let parent_base = ranges.try_read_cell(parent_address_cells, at + child.address_cells)?;
        let size = ranges.try_read_cell(
            child.size_cells,
            at + child.address_cells + parent_address_cells,
        )?;

        let window_size = u64::try_from(window.size).ok()?;
        let end = u64::try_from(window.offset)
            .ok()?
            .checked_sub(child_base)?
            .checked_add(window_size)
            .filter(|end| *end <= size)?;
        let base = parent_base.checked_add(end)? - window_size;

        Some(AddressSpace {
            offset: usize::try_from(base).ok()?,
            size: window.size,
        })
    })
}

fn admitted_pages(
    dt: &DeviceTree<'_>,
    excluded: &[DeviceWindow],
    window: AddressSpace,
    page_size: NonZeroUsize,
) -> Result<AddressSpace, SkippedWindow> {
    let pages = page_span(window, page_size).ok_or(SkippedWindow::Inadmissible)?;

    let shares_excluded_page = excluded
        .iter()
        .filter_map(|excluded| page_span(excluded.window, page_size))
        .any(|excluded| excluded.start() < pages.end() && pages.start() < excluded.end());
    if shares_excluded_page {
        return Err(SkippedWindow::SharesExcludedPage);
    }

    if !is_window_admissible(dt, pages) {
        return Err(SkippedWindow::Inadmissible);
    }

    Ok(pages)
}

fn page_span(window: AddressSpace, page_size: NonZeroUsize) -> Option<AddressSpace> {
    let start = window.offset - window.offset % page_size;
    let end = window
        .offset
        .checked_add(window.size)?
        .checked_next_multiple_of(page_size.get())?;

    Some(AddressSpace {
        offset: start,
        size: end - start,
    })
}

fn reg_entry_count(reg: &Property<'_>, cells: CellsSize) -> usize {
    (reg.value().len() / size_of::<u32>())
        .checked_div(cells.stride())
        .unwrap_or(0)
}

fn reg_entry(reg: &Property<'_>, cells: CellsSize, index: usize) -> Option<AddressSpace> {
    let at = index.checked_mul(cells.stride())?;
    let address = reg.try_read_cell(cells.address_cells, at)?;
    let size = reg.try_read_cell(cells.size_cells, at + cells.address_cells)?;

    Some(AddressSpace {
        offset: usize::try_from(address).ok()?,
        size: usize::try_from(size).ok()?,
    })
}
