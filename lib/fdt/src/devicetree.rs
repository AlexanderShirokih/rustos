#![allow(unsafe_code)]

use core::slice::from_raw_parts;

use crate::{
    cursor::{Cursor, cstr_at},
    devicetreeext::node_path,
};

/// Узел Device Tree.
#[derive(Clone, Copy)]
pub struct Node<'a> {
    name: &'a str,

    offset: usize,

    blocks: Blocks<'a>,
}

/// Уникальный ключ узла для сравнения и хранения.
pub type NodeKey = usize;

impl<'a> Node<'a> {
    pub const fn name(&self) -> &'a str {
        self.name
    }

    pub fn key(&self) -> NodeKey {
        self.offset
    }

    pub fn properties(&self) -> PropertyIter<'a> {
        let props_walker = DeviceTreeWalker::new(self.blocks, self.offset);

        PropertyIter {
            walker: props_walker,
        }
    }

    pub fn prop(&self, prop_name: &str) -> Option<Property<'a>> {
        self.properties().find(|p| p.name == prop_name)
    }

    pub fn children(&self) -> NodeIter<'a> {
        let node_walker = DeviceTreeWalker::new(self.blocks, self.offset);

        NodeIter {
            walker: node_walker,
        }
    }
}

/// Свойство (атрибут) узла Device Tree.
#[derive(Clone, Copy)]
pub struct Property<'a> {
    name: &'a str,

    value: &'a [u8],
}

impl<'a> Property<'a> {
    pub const fn name(&self) -> &'a str {
        self.name
    }

    pub fn value(&self) -> &'a [u8] {
        self.value
    }

    /// Возвращает байты значения до первого нуля как строку, как `of_property_read_string` в
    /// Linux. Значение без нуля и не в UTF-8 до первого нуля даёт `None`.
    pub fn as_cstr(&self) -> Option<&'a str> {
        let end = self.value.iter().position(|&byte| byte == 0)?;
        str::from_utf8(self.value.get(..end)?).ok()
    }

    pub fn try_as_u32(&self, offset: usize) -> Option<u32> {
        Some(u32::from_be_bytes(
            *self.value.get(offset..)?.first_chunk()?,
        ))
    }

    pub fn try_as_u64(&self, offset: usize) -> Option<u64> {
        Some(u64::from_be_bytes(
            *self.value.get(offset..)?.first_chunk()?,
        ))
    }

    /// Возвращает значение как `usize` (4 или 8 байт big-endian). Любая другая длина и
    /// значение, не умещающееся в `usize`, дают 0.
    pub fn as_usize(&self) -> usize {
        let bytes = self.value;

        match bytes.len() {
            4 => self.try_as_u32(0).map_or(0, |value| value as usize),

            8 => self
                .try_as_u64(0)
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or(0),

            _ => 0,
        }
    }
}

/// Flattened Device Tree (FDT) с блоком структуры, который конструктор проверил целиком.
pub struct DeviceTree<'a> {
    buffer: &'a [u8],

    header: FdtHeader,

    blocks: Blocks<'a>,
}

/// Ошибки разбора Device Tree.
#[derive(Debug, Clone, Copy)]
pub enum DtError {
    /// Неверная сигнатура (magic) в заголовке.
    InvalidMagic(u32),

    /// total_size в заголовке меньше заголовка своей версии, больше MAX_DTB_SIZE либо
    /// превышает длину переданного буфера.
    Incomplete {
        /// Ожидаемый размер из заголовка.
        total_size: usize,
        /// Размер проверенного буфера: 40 байт заголовка для `from_ptr`, длина среза
        /// для `from_bytes`.
        actual: usize,
    },

    /// Версия формата ниже 2 либо blob не читается разборщиком версии 17, последней
    /// поддерживаемой.
    UnsupportedVersion {
        /// Версия формата из заголовка.
        version: u32,
        /// Наименьшая версия, с которой blob совместим.
        last_compatible: u32,
    },

    /// Блок структуры либо строк выходит за `total_size`, токен неизвестен, имя или
    /// значение свойства выходит за свой блок, вложенность узлов не сбалансирована либо
    /// нет завершающего токена.
    Malformed {
        /// Смещение от начала блоба, на котором проверка обнаружила повреждение.
        offset: usize,
    },
}

impl<'a> DeviceTree<'a> {
    const MAX_DTB_SIZE: usize = 2 * 1024 * 1024;

    /// Разбирает FDT-blob по адресу `address`. Вызывающий гарантирует, что по этому адресу
    /// лежит валидный FDT и что память остаётся читаемой весь срок жизни результата. Срок
    /// не связан заимствованием ни с одним аргументом.
    pub fn from_ptr(address: usize) -> Result<Self, DtError> {
        if address == 0 {
            return Err(DtError::InvalidMagic(0));
        }

        let ptr_u8 = address as *const u8;

        // SAFETY: вызывающий гарантирует, что address указывает на валидный FDT-blob
        // размером не менее заголовка.
        let hdr_buf = unsafe { from_raw_parts(ptr_u8, FdtHeader::LONGEST_HEADER_SIZE) };

        let mut cursor = Cursor::new(hdr_buf);
        let header = FdtHeader::read_checked(&mut cursor)?;
        let total_size = header.total_size;

        // Ограничиваем MAX_DTB_SIZE, чтобы OOB-доступ к физической памяти был
        // невозможен независимо от содержимого заголовка.
        if total_size > Self::MAX_DTB_SIZE {
            return Err(DtError::Incomplete {
                total_size,
                actual: Self::MAX_DTB_SIZE,
            });
        }

        // SAFETY: total_size прошёл проверку (<= MAX_DTB_SIZE). Вызывающий
        // гарантирует, что буфер по address содержит как минимум total_size
        // подряд идущих читаемых байт.
        let buffer = unsafe { from_raw_parts(ptr_u8, total_size) };

        Self::checked(buffer, header)
    }

    /// Разбирает FDT-blob из `buffer`. Результат заимствует `buffer` на весь свой срок
    /// жизни.
    pub fn from_bytes(buffer: &'a [u8]) -> Result<Self, DtError> {
        let mut cur = Cursor::new(buffer);
        let header = FdtHeader::read_checked(&mut cur)?;

        // Защита от недоверенного blob: total_size из заголовка не должен
        // превышать фактический размер буфера.
        if header.total_size > buffer.len() {
            return Err(DtError::Incomplete {
                total_size: header.total_size,
                actual: buffer.len(),
            });
        }

        Self::checked(buffer, header)
    }

    fn checked(buffer: &'a [u8], header: FdtHeader) -> Result<Self, DtError> {
        let blocks = Blocks::of(buffer, &header)?;
        DeviceTreeWalker::new(blocks, header.struct_off as usize).check()?;

        Ok(DeviceTree {
            buffer,
            header,
            blocks,
        })
    }

    pub fn base_address(&self) -> usize {
        self.buffer.as_ptr() as usize
    }

    pub fn size(&self) -> usize {
        self.header.total_size
    }

    pub fn memory_reservations(&self) -> MemoryReservationIter<'a> {
        MemoryReservationIter {
            buffer: self.buffer,
            position: self.header.mem_rsvmap_off as usize,
        }
    }

    pub fn nodes(&'a self) -> NodeIter<'a> {
        match self.root() {
            Some(root) => root.children(),
            None => self.root_nodes(),
        }
    }

    /// Ищет узел по абсолютному пути, как [`node_path`], либо по имени в разделе `aliases`.
    /// Пример: алиасу `serial0` соответствует путь `/soc/serial@1000`.
    pub fn find(&'a self, path: &str) -> Option<Node<'a>> {
        // Пытаемся найти в списке узлов
        let node = self.find_node(path);
        if node.is_some() {
            return node;
        }

        // Попытка найти через alias
        self.find_by_alias(path)
    }

    fn find_by_alias(&'a self, alias: &str) -> Option<Node<'a>> {
        let root = self.root()?;
        let aliases = root.children().find(|n| n.name == "aliases")?;
        let prop = aliases.properties().find(|p| p.name == alias)?;
        let path = prop.as_cstr()?;

        self.find_node(path)
    }

    fn find_node(&'a self, path: &str) -> Option<Node<'a>> {
        node_path(self, path).map(|found| found.node())
    }

    /// Все узлы дерева в порядке описания вместе с глубиной. Глубина корня - ноль.
    pub fn walk(&'a self) -> TreeWalk<'a> {
        TreeWalk {
            walker: DeviceTreeWalker::new(self.blocks, self.header.struct_off as usize),
            depth: 0,
        }
    }

    pub fn root(&'a self) -> Option<Node<'a>> {
        self.root_nodes().next()
    }

    fn root_nodes(&'a self) -> NodeIter<'a> {
        let walker = DeviceTreeWalker::new(self.blocks, self.header.struct_off as usize);
        NodeIter { walker }
    }
}

/// Запись блока резервирования памяти FDT
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReservedRange {
    pub address: usize,
    pub size: usize,
}

/// Итератор по 16-байтовым записям блока резервирования (u64 BE address, u64 BE size).
pub struct MemoryReservationIter<'a> {
    buffer: &'a [u8],
    position: usize,
}

impl Iterator for MemoryReservationIter<'_> {
    type Item = ReservedRange;

    fn next(&mut self) -> Option<Self::Item> {
        let address = read_u64_be(self.buffer, self.position)?;
        let size = read_u64_be(self.buffer, self.position + size_of::<u64>())?;
        if address == 0 && size == 0 {
            return None;
        }

        self.position += 2 * size_of::<u64>();
        Some(ReservedRange {
            address: usize::try_from(address).ok()?,
            size: usize::try_from(size).ok()?,
        })
    }
}

fn read_u64_be(buffer: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_be_bytes(*buffer.get(offset..)?.first_chunk()?))
}

#[derive(Clone, Copy)]
struct FdtHeader {
    total_size: usize,

    struct_off: u32,

    strings_off: u32,

    mem_rsvmap_off: u32,

    strings_size: Option<u32>,

    struct_size: Option<u32>,
}

#[derive(Clone, Copy)]
struct Blocks<'a> {
    blob_through_structure: &'a [u8],

    strings: &'a [u8],
}

impl FdtHeader {
    const MAGIC: u32 = 0xD00D_FEED;

    const FIRST_VERSION: u32 = 2;

    const LAST_VERSION: u32 = 17;

    const LONGEST_HEADER_SIZE: usize = Self::size_of_version(Self::LAST_VERSION);

    fn read_checked(cur: &mut Cursor<'_>) -> Result<Self, DtError> {
        cur.set_position(0);

        let actual = cur.buffer.len();
        let mut read = || {
            cur.read_u32().ok_or(DtError::Incomplete {
                total_size: Self::LONGEST_HEADER_SIZE,
                actual,
            })
        };

        let magic = read()?;
        if magic != FdtHeader::MAGIC {
            return Err(DtError::InvalidMagic(magic));
        }

        let total_size = read()? as usize;
        let struct_off = read()?;
        let strings_off = read()?;
        let mem_rsvmap_off = read()?;
        let version = read()?;
        let last_compatible = read()?;
        if version < Self::FIRST_VERSION || last_compatible > Self::LAST_VERSION {
            return Err(DtError::UnsupportedVersion {
                version,
                last_compatible,
            });
        }

        // total_size из недоверенного blob ограничиваем диапазоном
        // [заголовок версии, MAX_DTB_SIZE].
        if !(Self::size_of_version(version)..=DeviceTree::MAX_DTB_SIZE).contains(&total_size) {
            return Err(DtError::Incomplete { total_size, actual });
        }

        let _boot_cpu = read()?;
        let strings_size = (version >= 3).then(&mut read).transpose()?;
        let struct_size = (version >= 17).then(&mut read).transpose()?;

        Ok(FdtHeader {
            total_size,
            struct_off,
            strings_off,
            mem_rsvmap_off,
            strings_size,
            struct_size,
        })
    }

    const fn size_of_version(version: u32) -> usize {
        let fields = match version {
            ..3 => 8,
            3..17 => 9,
            17.. => 10,
        };
        size_of::<u32>() * fields
    }
}

impl<'a> Blocks<'a> {
    fn of(buffer: &'a [u8], header: &FdtHeader) -> Result<Self, DtError> {
        let within = buffer.get(..header.total_size).ok_or(DtError::Incomplete {
            total_size: header.total_size,
            actual: buffer.len(),
        })?;
        let structure_start = header.struct_off as usize;
        let blob_through_structure = header
            .struct_size
            .map_or(Some(header.total_size), |size| {
                structure_start.checked_add(size as usize)
            })
            .and_then(|end| within.get(..end))
            .ok_or(DtError::Malformed {
                offset: structure_start,
            })?;
        let strings_start = header.strings_off as usize;
        let strings = header
            .strings_size
            .map_or(Some(header.total_size), |size| {
                strings_start.checked_add(size as usize)
            })
            .and_then(|end| within.get(strings_start..end))
            .ok_or(DtError::Malformed {
                offset: strings_start,
            })?;

        Ok(Self {
            blob_through_structure,
            strings,
        })
    }
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NodeToken {
    BeginNode,
    EndNode,
    Property,
    Nop,
    End,
}

impl NodeToken {
    fn from_u32(v: u32) -> Option<Self> {
        match v {
            0x1 => Some(NodeToken::BeginNode),
            0x2 => Some(NodeToken::EndNode),
            0x3 => Some(NodeToken::Property),
            0x4 => Some(NodeToken::Nop),
            0x9 => Some(NodeToken::End),
            _ => None,
        }
    }
}

enum AbstractNode<'a> {
    BeginNode { name: &'a str, offset: usize },
    EndNode,
    End,
    Property(Property<'a>),
}

enum ValueNode<'a> {
    Node(Node<'a>),
    Property(Property<'a>),
}

struct DeviceTreeWalker<'a> {
    cursor: Cursor<'a>,
    blocks: Blocks<'a>,
    next_pos: usize,
    level: i32,
}

impl<'a> DeviceTreeWalker<'a> {
    fn new(blocks: Blocks<'a>, offset: usize) -> Self {
        Self {
            cursor: Cursor::new(blocks.blob_through_structure),
            blocks,
            next_pos: offset,
            level: 0,
        }
    }

    fn check(mut self) -> Result<(), DtError> {
        let mut root_closed = false;
        loop {
            let malformed = DtError::Malformed {
                offset: self.next_pos,
            };
            match self.step()? {
                AbstractNode::BeginNode { .. } if root_closed => return Err(malformed),
                AbstractNode::BeginNode { .. } => self.level += 1,
                AbstractNode::EndNode if self.level == 0 => return Err(malformed),
                AbstractNode::EndNode => {
                    self.level -= 1;
                    root_closed = self.level == 0;
                }
                AbstractNode::Property(_) if self.level > 0 => {}
                AbstractNode::End if root_closed => return Ok(()),
                AbstractNode::Property(_) | AbstractNode::End => return Err(malformed),
            }
        }
    }

    fn next_value(&mut self) -> Option<ValueNode<'a>> {
        loop {
            match self.step().ok()? {
                AbstractNode::BeginNode { name, offset } => {
                    if self.level > 0 {
                        self.level += 1;
                        continue;
                    }

                    self.level += 1;
                    return Some(ValueNode::Node(Node {
                        name,
                        offset,
                        blocks: self.blocks,
                    }));
                }

                AbstractNode::EndNode => {
                    if self.level > 0 {
                        self.level -= 1;
                    } else {
                        return None;
                    }
                }

                AbstractNode::End => {
                    return None;
                }

                AbstractNode::Property(property) => {
                    if self.level == 0 {
                        return Some(ValueNode::Property(property));
                    }
                }
            }
        }
    }

    fn step(&mut self) -> Result<AbstractNode<'a>, DtError> {
        let node = self.read_abstract_node(self.next_pos)?;
        self.cursor.align_up4();
        self.next_pos = self.cursor.position();
        Ok(node)
    }

    fn read_abstract_node(&mut self, offset: usize) -> Result<AbstractNode<'a>, DtError> {
        self.cursor.set_position(offset);

        loop {
            let malformed = DtError::Malformed {
                offset: self.cursor.position(),
            };
            let token = self
                .cursor
                .read_u32()
                .and_then(NodeToken::from_u32)
                .ok_or(malformed)?;

            match token {
                NodeToken::BeginNode => {
                    let name = self.cursor.read_cstr_here().ok_or(malformed)?;
                    self.cursor.align_up4();
                    return Ok(AbstractNode::BeginNode {
                        name,
                        offset: self.cursor.position(),
                    });
                }

                NodeToken::EndNode => {
                    return Ok(AbstractNode::EndNode);
                }

                NodeToken::End => {
                    return Ok(AbstractNode::End);
                }

                NodeToken::Property => {
                    return self
                        .read_property()
                        .map(AbstractNode::Property)
                        .ok_or(malformed);
                }

                NodeToken::Nop => {}
            }
        }
    }

    fn read_property(&mut self) -> Option<Property<'a>> {
        let length = self.cursor.read_u32()? as usize;
        let name_offset = self.cursor.read_u32()? as usize;
        let name = cstr_at(self.blocks.strings, name_offset)?;
        let value = self.cursor.read_bytes(length)?;
        Some(Property { name, value })
    }
}

/// Итератор по дочерним узлам.
pub struct NodeIter<'a> {
    walker: DeviceTreeWalker<'a>,
}

impl<'a> Iterator for NodeIter<'a> {
    type Item = Node<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.walker.next_value()? {
                ValueNode::Node(n) => return Some(n),
                ValueNode::Property(_) => {}
            }
        }
    }
}

/// Итератор узлов дерева с глубиной, который строит [`DeviceTree::walk`].
pub struct TreeWalk<'a> {
    walker: DeviceTreeWalker<'a>,
    depth: usize,
}

impl<'a> Iterator for TreeWalk<'a> {
    type Item = (usize, Node<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.walker.step().ok()? {
                AbstractNode::BeginNode { name, offset } => {
                    let depth = self.depth;
                    self.depth += 1;
                    let node = Node {
                        name,
                        offset,
                        blocks: self.walker.blocks,
                    };
                    return Some((depth, node));
                }
                AbstractNode::EndNode => self.depth = self.depth.checked_sub(1)?,
                AbstractNode::Property(_) => {}
                AbstractNode::End => return None,
            }
        }
    }
}

pub struct PropertyIter<'a> {
    walker: DeviceTreeWalker<'a>,
}

impl<'a> Iterator for PropertyIter<'a> {
    type Item = Property<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.walker.next_value()? {
                ValueNode::Property(p) => return Some(p),
                ValueNode::Node(_) => {}
            }
        }
    }
}
