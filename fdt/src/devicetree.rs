use crate::cursor::Cursor;
use core::ops::Sub;
use core::slice::from_raw_parts;

/// Узел дерева
#[derive(Clone, Copy)]
pub struct Node<'a> {
    name: &'a str,
    offset: usize,
    header: &'a FdtHeader,
    buffer: &'a [u8],
}

#[derive(Clone, Ord, PartialOrd, Eq, PartialEq, Debug)]
pub struct NodeKey {
    offset: usize,
}

impl<'a> Node<'a> {
    pub const fn name(&self) -> &'a str {
        self.name
    }

    pub fn key(&self) -> NodeKey {
        NodeKey {
            offset: self.offset,
        }
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

/// Аттрибут узла дерева
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

    /// Возвращает value интерпретируя как &str
    pub fn as_cstr(&self) -> Option<&'a str> {
        let bytes = self.value;
        let len = bytes.len().sub(1);

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

    /// Возвращает value интерпретируя как usize
    pub fn as_usize(&self) -> usize {
        let bytes = self.value;

        match bytes.len() {
            4 => self.try_as_u32(0).unwrap_or(0) as usize,

            8 => self.try_as_u64(0).unwrap_or(0) as usize,

            _ => 0,
        }
    }
}

pub struct DeviceTree<'a> {
    buffer: &'a [u8],
    header: FdtHeader,
}

/// Ошибки разбора Device Tree
#[derive(Debug)]
pub enum DtError {
    InvalidMagic(u32),
    Incomplete { total_size: usize, actual: usize },
}

impl<'a> DeviceTree<'a> {
    pub fn from_ptr(address: usize) -> Result<Self, DtError> {
        if address == 0 {
            return Err(DtError::InvalidMagic(0));
        }

        let ptr_u8 = address as *const u8;

        let hdr_buf = unsafe { from_raw_parts(ptr_u8, size_of::<u32>() * 4) };

        let mut cursor = Cursor::new(hdr_buf);
        let header = FdtHeader::read_checked(&mut cursor)?;
        let total_size = header.total_size;

        let buffer = unsafe { from_raw_parts(ptr_u8, total_size) };

        Ok(DeviceTree { buffer, header })
    }

    pub fn from_bytes(buffer: &'a [u8]) -> Result<Self, DtError> {
        let mut cur = Cursor::new(buffer);
        let header = FdtHeader::read_checked(&mut cur)?;
        Ok(DeviceTree { buffer, header })
    }

    pub fn base_address(&self) -> usize {
        self.buffer.as_ptr() as usize
    }

    pub fn size(&self) -> usize {
        self.header.total_size
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

        // Пытаемся найти через alias
        self.find_by_alias(path)
    }

    /// Возвращает узел по названию псевдонима (секция aliases)
    /// Например: find_by_alias("serial10")
    pub fn find_by_alias(&'a self, alias: &str) -> Option<Node<'a>> {
        let root = self.root()?;
        let aliases = root.children().find(|n| n.name == "aliases")?;
        let prop = aliases.properties().find(|p| p.name == alias)?;
        let path = prop.as_cstr()?;

        self.find_node(path)
    }

    /// Ищет узел в списке структур. Принимает только абсолютный путь
    /// Например: find("/soc@107c000000/serial@7d001000")
    pub fn find_node(&'a self, path: &str) -> Option<Node<'a>> {
        // Быстрый путь. "/" → корень
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

#[derive(Clone, Copy)]
struct FdtHeader {
    /// Размер структуры
    total_size: usize,

    /// Сдвиг от начала буфера до начала блока структур
    struct_off: u32,

    /// Сдвиг от начала буфера до пула строковых констант
    strings_off: u32,
}

impl FdtHeader {
    const MAGIC: u32 = 0xD00D_FEED;

    fn read_checked(cur: &mut Cursor<'_>) -> Result<Self, DtError> {
        cur.set_position(0);

        let magic = cur.read_u32();
        if magic != FdtHeader::MAGIC {
            return Err(DtError::InvalidMagic(magic));
        }

        let total_size = cur.read_u32() as usize;

        let struct_off = cur.read_u32();
        let strings_off = cur.read_u32();

        Ok(FdtHeader {
            total_size,
            struct_off,
            strings_off,
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
                        continue;
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
                    continue;
                }
            }
        }
    }

    fn read_abstract_node(&mut self, offset: usize) -> Option<AbstractNode<'a>> {
        self.cursor.set_position(offset);

        loop {
            let tok_u32 = self.cursor.read_u32();
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
                    let length = self.cursor.read_u32() as usize;
                    let name_off = self.cursor.read_u32() as usize;
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
                    continue;
                }
            }
        }
    }

    fn map_begin_node(&self, name: &'a str, offset: usize) -> ValueNode<'a> {
        ValueNode::Node(Node::<'a> {
            name,
            offset,
            header: &self.header,
            buffer: &self.cursor.buffer,
        })
    }

    fn map_property(&self, name: &'a str, offset: usize, length: usize) -> ValueNode<'a> {
        let start = offset;
        let end = start + length;
        let value = &self.cursor.buffer[start..end];
        ValueNode::Property(Property { name, value })
    }
}

pub struct NodeIter<'a> {
    walker: DeviceTreeWalker<'a>,
}

impl<'a> Iterator for NodeIter<'a> {
    type Item = Node<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.walker.next_value()? {
                ValueNode::Node(n) => return Some(n),
                ValueNode::Property(_) => continue,
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
                ValueNode::Node(_) => continue,
            }
        }
    }
}
