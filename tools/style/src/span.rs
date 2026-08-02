/// Полуинтервал байтовых смещений в тексте файла.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Span {
    /// Смещение первого байта.
    pub start: u32,
    /// Смещение за последним байтом.
    pub end: u32,
}

impl Span {
    /// Собирает диапазон из пары смещений.
    pub fn new(start: u32, end: u32) -> Span {
        Span { start, end }
    }

    /// Сообщает, лежит ли другой диапазон внутри этого.
    pub fn contains(self, other: Span) -> bool {
        self.start <= other.start && other.end <= self.end
    }

    /// Сообщает, пересекаются ли диапазоны хотя бы одним байтом.
    pub fn overlaps(self, other: Span) -> bool {
        self.start < other.end && other.start < self.end
    }
}

/// Позиция в файле: строка и колонка от единицы, колонка в символах.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Position {
    /// Номер строки от единицы.
    pub line: u32,
    /// Номер колонки от единицы, в символах.
    pub column: u32,
}

/// Отображение байтового смещения в позицию.
pub struct LineIndex {
    starts: Vec<u32>,
}

impl LineIndex {
    /// Строит индекс по тексту файла.
    pub fn new(text: &str) -> LineIndex {
        let mut starts = vec![0u32];
        for (offset, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                starts.push(offset as u32 + 1);
            }
        }
        LineIndex { starts }
    }

    /// Возвращает номер строки от нуля, содержащей смещение.
    pub fn line_of(&self, offset: u32) -> usize {
        match self.starts.binary_search(&offset) {
            Ok(index) => index,
            Err(index) => index.saturating_sub(1),
        }
    }

    /// Возвращает смещение начала строки, заданной номером от нуля.
    pub fn line_start(&self, line: usize) -> u32 {
        self.starts.get(line).copied().unwrap_or_default()
    }

    /// Возвращает число строк.
    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    /// Возвращает диапазон строки без завершающего перевода строки.
    pub fn line_span(&self, text: &str, line: usize) -> Span {
        let start = self.line_start(line);
        let end = if line + 1 < self.line_count() {
            self.line_start(line + 1)
        } else {
            text.len() as u32
        };
        let body = &text[start as usize..end as usize];
        let trimmed = body.trim_end_matches(['\n', '\r']);
        Span::new(start, start + trimmed.len() as u32)
    }

    /// Переводит смещение в позицию.
    pub fn position(&self, text: &str, offset: u32) -> Position {
        let line = self.line_of(offset);
        let start = self.line_start(line) as usize;
        let head = text.get(start..offset as usize).unwrap_or_default();
        Position {
            line: line as u32 + 1,
            column: head.chars().count() as u32 + 1,
        }
    }
}
