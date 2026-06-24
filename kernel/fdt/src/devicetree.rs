#![allow(unsafe_code)]

use core::slice::from_raw_parts;

use crate::cursor::Cursor;

/// Узел Device Tree.
#[derive(Clone, Copy)]
pub struct Node<'a> {
    /// Имя узла (например, "serial@7d001000").
    name: &'a str,

    /// Смещение содержимого узла в буфере.
    offset: usize,

    /// Ссылка на заголовок FDT.
    header: &'a FdtHeader,

    /// Буфер с данными всего дерева.
    buffer: &'a [u8],
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
        let props_walker = DeviceTreeWalker::new(self.buffer, self.header, self.offset);

        PropertyIter {
            walker: props_walker,
        }
    }

    pub fn prop(&self, prop_name: &str) -> Option<Property<'a>> {
        self.properties().find(|p| p.name == prop_name)
    }

    pub fn children(&self) -> NodeIter<'a> {
        let node_walker = DeviceTreeWalker::new(self.buffer, self.header, self.offset);

        NodeIter {
            walker: node_walker,
        }
    }
}

/// Свойство (атрибут) узла Device Tree.
#[derive(Clone, Copy)]
pub struct Property<'a> {
    /// Имя свойства.
    name: &'a str,

    /// Сырые байты значения свойства.
    value: &'a [u8],
}

impl<'a> Property<'a> {
    pub const fn name(&self) -> &'a str {
        self.name
    }

    pub fn value(&self) -> &'a [u8] {
        self.value
    }

    /// Возвращает значение как строку (без терминального нуля).
    pub fn as_cstr(&self) -> Option<&'a str> {
        let bytes = self.value;
        let len = bytes.len().saturating_sub(1);

        str::from_utf8(&bytes[..len]).ok()
    }

    pub fn try_as_u32(&self, offset: usize) -> Option<u32> {
        let bytes = self.value;
        let slice = bytes.get(offset..offset + size_of::<u32>())?;
        Some(u32::from_be_bytes(slice.try_into().unwrap()))
    }

    pub fn try_as_u64(&self, offset: usize) -> Option<u64> {
        let bytes = self.value;
        let slice = bytes.get(offset..offset + size_of::<u64>())?;
        Some(u64::from_be_bytes(slice.try_into().unwrap()))
    }

    /// Возвращает значение как `usize` (4 или 8 байт big-endian).
    pub fn as_usize(&self) -> usize {
        let bytes = self.value;

        match bytes.len() {
            4 => self.try_as_u32(0).unwrap_or(0) as usize,

            8 => self.try_as_u64(0).unwrap_or(0) as usize,

            _ => 0,
        }
    }
}

/// Flattened Device Tree (FDT).
pub struct DeviceTree<'a> {
    /// Буфер с бинарными данными дерева.
    buffer: &'a [u8],

    /// Разобранный заголовок FDT.
    header: FdtHeader,
}

/// Ошибки разбора Device Tree.
#[derive(Debug)]
pub enum DtError {
    /// Неверная сигнатура (magic) в заголовке.
    InvalidMagic(u32),

    /// total_size в заголовке выходит за пределы MAX_DTB_SIZE или превышает
    /// длину переданного буфера.
    Incomplete {
        /// Ожидаемый размер из заголовка.
        total_size: usize,
        /// Фактический размер буфера.
        actual: usize,
    },
}

impl<'a> DeviceTree<'a> {
    /// Верхний предел total_size из недоверенного FDT-blob.
    const MAX_DTB_SIZE: usize = 2 * 1024 * 1024;

    pub fn from_ptr(address: usize) -> Result<Self, DtError> {
        if address == 0 {
            return Err(DtError::InvalidMagic(0));
        }

        let ptr_u8 = address as *const u8;

        // SAFETY: вызывающий гарантирует, что address указывает на валидный FDT-blob
        // размером не менее 24 байт (6 x u32 заголовка).
        let hdr_buf = unsafe { from_raw_parts(ptr_u8, size_of::<u32>() * 6) };

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

        Ok(DeviceTree { buffer, header })
    }

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

        Ok(DeviceTree { buffer, header })
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

    /// Ищет узел в списке структур. Принимает как абсолютный путь, так и alias
    /// Например: find("serial10"), find("/soc@107c000000/serial@7d001000")
    pub fn find(&'a self, path: &str) -> Option<Node<'a>> {
        // Пытаемся найти в списке узлов
        let node = self.find_node(path);
        if node.is_some() {
            return node;
        }

        // Попытка найти через alias
        self.find_by_alias(path)
    }

    /// Возвращает узел по названию псевдонима (секция aliases)
    fn find_by_alias(&'a self, alias: &str) -> Option<Node<'a>> {
        let root = self.root()?;
        let aliases = root.children().find(|n| n.name == "aliases")?;
        let prop = aliases.properties().find(|p| p.name == alias)?;
        let path = prop.as_cstr()?;

        self.find_node(path)
    }

    /// Ищет узел в списке структур. Принимает только абсолютный путь
    /// Например: find("/soc@107c000000/serial@7d001000")
    fn find_node(&'a self, path: &str) -> Option<Node<'a>> {
        // Быстрый путь. "/" -> корень
        if path == "/" {
            return self.root();
        }

        let components = path.split('/').filter(|s| !s.is_empty());
        let mut node = self.root()?;

        for name in components {
            node = node.children().find(|child| child.name == name)?;
        }

        Some(node)
    }

    pub fn root(&'a self) -> Option<Node<'a>> {
        self.root_nodes().next()
    }

    fn root_nodes(&'a self) -> NodeIter<'a> {
        let walker =
            DeviceTreeWalker::new(self.buffer, &self.header, self.header.struct_off as usize);
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
            address: address as usize,
            size: size as usize,
        })
    }
}

fn read_u64_be(buffer: &[u8], offset: usize) -> Option<u64> {
    let slice = buffer.get(offset..offset + size_of::<u64>())?;
    Some(u64::from_be_bytes(slice.try_into().unwrap()))
}

/// Заголовок Flattened Device Tree.
#[derive(Clone, Copy)]
struct FdtHeader {
    /// Полный размер FDT в байтах.
    total_size: usize,

    /// Смещение блока структур от начала буфера.
    struct_off: u32,

    /// Смещение пула строк от начала буфера.
    strings_off: u32,

    /// Смещение блока резервирования памяти от начала буфера.
    mem_rsvmap_off: u32,
}

impl FdtHeader {
    /// Магическое число FDT.
    const MAGIC: u32 = 0xD00D_FEED;

    /// Минимальный размер заголовка: 6 x u32 = 24 байта.
    const HEADER_MIN_SIZE: usize = size_of::<u32>() * 6;

    fn read_checked(cur: &mut Cursor<'_>) -> Result<Self, DtError> {
        cur.set_position(0);

        let actual = cur.buffer.len();
        let mut read = || {
            cur.read_u32().ok_or(DtError::Incomplete {
                total_size: Self::HEADER_MIN_SIZE,
                actual,
            })
        };

        let magic = read()?;
        if magic != FdtHeader::MAGIC {
            return Err(DtError::InvalidMagic(magic));
        }

        // total_size из недоверенного blob ограничиваем диапазоном
        // [HEADER_MIN_SIZE, MAX_DTB_SIZE].
        let total_size = read()? as usize;
        if !(Self::HEADER_MIN_SIZE..=DeviceTree::MAX_DTB_SIZE).contains(&total_size) {
            return Err(DtError::Incomplete { total_size, actual });
        }

        let struct_off = read()?;
        let strings_off = read()?;
        let mem_rsvmap_off = read()?;

        Ok(FdtHeader {
            total_size,
            struct_off,
            strings_off,
            mem_rsvmap_off,
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
    BeginNode {
        name: &'a str,
        offset: usize,
    },
    EndNode,
    End,
    Property {
        name: &'a str,
        offset: usize,
        length: usize,
    },
}

enum ValueNode<'a> {
    Node(Node<'a>),
    Property(Property<'a>),
}

struct DeviceTreeWalker<'a> {
    cursor: Cursor<'a>,
    header: &'a FdtHeader,
    next_pos: usize,
    level: i32,
}

impl<'a> DeviceTreeWalker<'a> {
    fn new(buf: &'a [u8], header: &'a FdtHeader, offset: usize) -> Self {
        Self {
            cursor: Cursor::new(buf),
            header,
            next_pos: offset,
            level: 0,
        }
    }

    fn next_value(&mut self) -> Option<ValueNode<'a>> {
        loop {
            let node = self.read_abstract_node(self.next_pos)?;
            self.cursor.align_up4();

            self.next_pos = self.cursor.position();

            match node {
                AbstractNode::BeginNode { name, offset } => {
                    if self.level > 0 {
                        self.level += 1;
                        continue;
                    }

                    self.level += 1;
                    return Some(self.map_begin_node(name, offset));
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

                AbstractNode::Property {
                    name,
                    offset,
                    length,
                } => {
                    if self.level == 0 {
                        return Some(self.map_property(name, offset, length));
                    }
                }
            }
        }
    }

    fn read_abstract_node(&mut self, offset: usize) -> Option<AbstractNode<'a>> {
        self.cursor.set_position(offset);

        loop {
            let tok_u32 = self.cursor.read_u32()?;
            let token = NodeToken::from_u32(tok_u32)?;

            match token {
                NodeToken::BeginNode => {
                    let name = self.cursor.read_cstr_here();
                    let off = {
                        self.cursor.align_up4();
                        self.cursor.position()
                    };
                    return Some(AbstractNode::BeginNode::<'a> { name, offset: off });
                }

                NodeToken::EndNode => {
                    return Some(AbstractNode::EndNode);
                }

                NodeToken::End => {
                    return Some(AbstractNode::End);
                }

                NodeToken::Property => {
                    let length = self.cursor.read_u32()? as usize;
                    let name_off = self.cursor.read_u32()? as usize;
                    let value_offset = self.cursor.position();
                    let name = self
                        .cursor
                        .read_cstr_at(self.header.strings_off as usize + name_off);

                    self.cursor.set_position(value_offset + length);

                    return Some(AbstractNode::Property::<'a> {
                        name,
                        offset: value_offset,
                        length,
                    });
                }

                NodeToken::Nop => {
                    // пропускаем и читаем следующий токен
                }
            }
        }
    }

    fn map_begin_node(&self, name: &'a str, offset: usize) -> ValueNode<'a> {
        ValueNode::Node(Node::<'a> {
            name,
            offset,
            header: self.header,
            buffer: self.cursor.buffer,
        })
    }

    fn map_property(&self, name: &'a str, offset: usize, length: usize) -> ValueNode<'a> {
        let buf = self.cursor.buffer;
        // Защита от завышенного `length`: ограничиваем срез
        // фактическими границами буфера вместо паники на OOB.
        let start = offset.min(buf.len());
        let end = start.saturating_add(length).min(buf.len());
        let value = &buf[start..end];
        ValueNode::Property(Property { name, value })
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
