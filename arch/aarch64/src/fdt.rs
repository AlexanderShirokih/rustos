use core::mem::size_of;

pub const FDT_MAGIC: u32 = 0xD00D_FEED;

#[inline(always)]
pub fn be32(x: u32) -> u32 {
    u32::from_be(x)
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FdtHeader {
    pub magic: u32,
    pub totalsize: u32,
    pub off_dt_struct: u32,
    pub off_dt_strings: u32,
    pub off_mem_rsvmap: u32,
    pub version: u32,
    pub last_comp_version: u32,
    pub boot_cpuid_phys: u32,
    pub size_dt_strings: u32,
    pub size_dt_struct: u32,
}

#[repr(u32)]
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum Token {
    FdtBeginNode = 1,
    FdtEndNode = 2,
    FdtProp = 3,
    FdtNop = 4,
    FdtEnd = 9,
}

#[inline(always)]
pub fn align4(v: usize) -> usize {
    // FDT требует выравнивания разделов struct/strings по 4 байта
    (v + 3) & !3
}

// ---------------- Высокоуровневый zero-copy API парсера ----------------

/// Корневой объект «живого» представления FDT.
/// Держит смещения на секции `structure` и `strings`.
pub struct DeviceTree {
    struct_base: usize,
    strings_base: usize,
}

/// Узел дерева: имя + границы тела (для итераторов).
#[derive(Clone, Copy)]
pub struct Node<'a> {
    dt: &'a DeviceTree,
    name: &'a [u8],
    body_off: usize,
    end_off: usize,
}

/// Свойство (имя + байтовое значение) без копирования.
#[derive(Clone, Copy)]
pub struct Prop<'a> {
    pub name: &'a [u8],
    pub value: &'a [u8],
}

impl DeviceTree {
    /// Создаёт `DeviceTree` из адреса DTB.
    /// `dtb_ptr` должен указывать на валидный, доступный для чтения blob FDT.
    pub unsafe fn from_ptr(dtb_ptr: usize) -> Option<Self> {
        if dtb_ptr == 0 {
            return None;
        }
        let hdr = unsafe { &*(dtb_ptr as *const FdtHeader) };

        if be32(hdr.magic) != FDT_MAGIC {
            return None;
        }
        let struct_off = be32(hdr.off_dt_struct) as usize;
        let strings_off = be32(hdr.off_dt_strings) as usize;
        Some(DeviceTree {
            struct_base: dtb_ptr + struct_off,
            strings_base: dtb_ptr + strings_off,
        })
    }

    /// Возвращает корневой узел `/` (если blob корректен).
    pub fn root(&'_ self) -> Option<Node<'_>> {
        let mut p = self.struct_base;
        let token = be32(unsafe { *(p as *const u32) });
        if token != Token::FdtBeginNode as u32 {
            return None;
        }
        p += size_of::<u32>();

        // Имя узла — C-строка, затем выравнивание на 4.
        let mut q = p;
        while unsafe { *(q as *const u8) } != 0 {
            q += 1;
        }
        let name = unsafe { core::slice::from_raw_parts(p as *const u8, q - p) };
        let body = align4(q + 1);

        // Найти границу текущего узла (сканируя вложенность по токенам).
        let end = self.scan_node_end(body);
        Some(Node {
            dt: self,
            name,
            body_off: body,
            end_off: end,
        })
    }

    /// Пролистывает поток токенов начиная с `p`, чтобы найти конец текущего узла.
    /// Поддерживает вложенность через счётчик `depth`.
    fn scan_node_end(&self, mut p: usize) -> usize {
        let mut depth = 1i32;
        loop {
            let tok = be32(unsafe { *(p as *const u32) });
            p += size_of::<u32>();
            match tok {
                t if t == Token::FdtBeginNode as u32 => {
                    // пропустить имя узла (C-строка + align)
                    let mut q = p;
                    while unsafe { *(q as *const u8) } != 0 {
                        q += 1;
                    }
                    p = align4(q + 1);
                    depth += 1;
                }
                t if t == Token::FdtEndNode as u32 => {
                    depth -= 1;
                    if depth == 0 {
                        return p;
                    }
                }
                t if t == Token::FdtProp as u32 => {
                    // len + nameoff + data[len] + align
                    let len = be32(unsafe { *(p as *const u32) }) as usize;
                    p += size_of::<u32>();
                    let _nameoff = be32(unsafe { *(p as *const u32) }) as usize;
                    p += size_of::<u32>();
                    p = align4(p + len);
                }
                t if t == Token::FdtNop as u32 => {}
                t if t == Token::FdtEnd as u32 => {
                    // Защита от битых blob'ов: выходим по End.
                    return p;
                }
                _ => {
                    // Неизвестный токен — считаем конец во избежание зацикливания.
                    return p;
                }
            }
        }
    }

    /// Глубокий поиск: обходит дерево в глубину и возвращает первый узел,
    /// для которого `pred(node)` истина.
    pub fn find_first<'a, F>(&'a self, mut pred: F) -> Option<Node<'a>>
    where
        F: FnMut(&Node<'a>) -> bool,
    {
        fn dfs<'a, F>(node: &Node<'a>, pred: &mut F) -> Option<Node<'a>>
        where
            F: FnMut(&Node<'a>) -> bool,
        {
            if pred(node) {
                return Some(*node);
            }
            let mut it = node.children();
            while let Some(child) = it.next() {
                if let Some(found) = dfs(&child, pred) {
                    return Some(found);
                }
            }
            None
        }

        self.root().and_then(|root| dfs(&root, &mut pred))
    }

    /// Возвращает C-строку из секции `strings` по смещению `off`.
    #[inline(always)]
    fn cstr_at(&self, off: usize) -> &[u8] {
        let base = self.strings_base as *const u8;
        let mut p = (base as usize) + off;
        let start = p as *const u8;

        unsafe {
            while *(p as *const u8) != 0 {
                p += 1;
            }
            core::slice::from_raw_parts(start, p - start as usize)
        }
    }
}

impl<'a> Node<'a> {
    /// Имя узла (без завершающего `\0`).
    pub fn name(&self) -> &'a [u8] {
        self.name
    }

    /// Итератор по свойствам **только этого** узла (вложенные узлы пропускаются).
    pub fn props(&self) -> PropsIter<'a> {
        PropsIter {
            node: *self,
            cur: self.body_off,
            depth: 1,
        }
    }

    /// Итератор по **прямым дочерним** узлам.
    pub fn children(&self) -> ChildrenIter<'a> {
        ChildrenIter {
            node: *self,
            cur: self.body_off,
            depth: 1,
        }
    }

    /// Возвращает значение свойства по имени (байтовое сравнение).
    pub fn get_prop(&self, want: &[u8]) -> Option<&'a [u8]> {
        let mut it = self.props();
        while let Some(p) = it.next() {
            if p.name == want {
                return Some(p.value);
            }
        }
        None
    }
}

/// Итератор по свойствам узла с пропуском вложенных узлов.
/// Держит «курсор» внутри секции `structure` и счётчик глубины.
pub struct PropsIter<'a> {
    node: Node<'a>,
    cur: usize,
    depth: i32,
}

impl<'a> PropsIter<'a> {
    fn bump_until_next(&mut self) -> Option<Prop<'a>> {
        while self.cur < self.node.end_off {
            let tok = be32(unsafe { *(self.cur as *const u32) });
            self.cur += size_of::<u32>();
            match tok {
                t if t == Token::FdtBeginNode as u32 => {
                    // пропускаем имя узла, углубляемся
                    let mut q = self.cur;
                    while unsafe { *(q as *const u8) } != 0 {
                        q += 1;
                    }
                    self.cur = align4(q + 1);
                    self.depth += 1;
                }
                t if t == Token::FdtEndNode as u32 => {
                    self.depth -= 1;
                    if self.depth == 0 {
                        return None;
                    }
                }
                t if t == Token::FdtProp as u32 => {
                    // читаем len/nameoff/data и, если на нужной глубине, отдаём Prop
                    let len = be32(unsafe { *(self.cur as *const u32) }) as usize;
                    self.cur += size_of::<u32>();
                    let nameoff = be32(unsafe { *(self.cur as *const u32) }) as usize;
                    self.cur += size_of::<u32>();
                    let data = self.cur as *const u8;
                    self.cur = align4(self.cur + len);
                    if self.depth == 1 {
                        let name = self.node.dt.cstr_at(nameoff);
                        let value = unsafe { core::slice::from_raw_parts(data, len) };
                        return Some(Prop { name, value });
                    }
                }
                t if t == Token::FdtNop as u32 => {}
                t if t == Token::FdtEnd as u32 => {
                    return None;
                }
                _ => {
                    return None;
                }
            }
        }
        None
    }
}

impl<'a> Iterator for PropsIter<'a> {
    type Item = Prop<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        self.bump_until_next()
    }
}

/// Итератор по дочерним узлам.
/// На глубине 1 вычисляет границу узла через `scan_node_end` и «перепрыгивает» его целиком.
pub struct ChildrenIter<'a> {
    node: Node<'a>,
    cur: usize,
    depth: i32,
}

impl<'a> ChildrenIter<'a> {
    fn bump_until_next(&mut self) -> Option<Node<'a>> {
        while self.cur < self.node.end_off {
            let tok = be32(unsafe { *(self.cur as *const u32) });
            self.cur += size_of::<u32>();
            match tok {
                t if t == Token::FdtBeginNode as u32 => {
                    // имя узла до '\0', затем тело с 4-байтовым выравниванием
                    let mut q = self.cur;
                    while unsafe { *(q as *const u8) } != 0 {
                        q += 1;
                    }
                    let name =
                        unsafe { core::slice::from_raw_parts(self.cur as *const u8, q - self.cur) };
                    let body = align4(q + 1);
                    if self.depth == 1 {
                        // На нужной глубине создаём дочерний Node и сразу перескакиваем на его конец,
                        // чтобы следующая итерация вернула «соседа».
                        let end = self.node.dt.scan_node_end(body);
                        let child = Node {
                            dt: self.node.dt,
                            name,
                            body_off: body,
                            end_off: end,
                        };
                        self.cur = end;
                        return Some(child);
                    } else {
                        // Иначе просто углубляемся внутрь
                        self.cur = body;
                    }
                    self.depth += 1;
                }
                t if t == Token::FdtEndNode as u32 => {
                    self.depth -= 1;
                    if self.depth == 0 {
                        return None;
                    }
                }
                t if t == Token::FdtProp as u32 => {
                    // пропускаем свойство (len + nameoff + data + align)
                    let len = be32(unsafe { *(self.cur as *const u32) }) as usize;
                    self.cur += size_of::<u32>();
                    let _nameoff = be32(unsafe { *(self.cur as *const u32) }) as usize;
                    self.cur += size_of::<u32>();
                    self.cur = align4(self.cur + len);
                }
                t if t == Token::FdtNop as u32 => {}
                t if t == Token::FdtEnd as u32 => {
                    return None;
                }
                _ => {
                    return None;
                }
            }
        }
        None
    }
}

impl<'a> Iterator for ChildrenIter<'a> {
    type Item = Node<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        self.bump_until_next()
    }
}
