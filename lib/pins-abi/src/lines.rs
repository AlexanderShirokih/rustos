use alloc::{collections::BTreeMap, string::String, vec::Vec};

use platform_abi::DeviceId;

use crate::error::PinsError;

/// Направление вывода линии.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Вывод читает уровень.
    Input,
    /// Вывод задаёт уровень.
    Output,
}

/// Запись описания, по которой выдана линия.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Record<'a> {
    /// Запись линии с именем из биндинга устройства, пустое имя называет безымянную линию.
    Line(&'a str),
    /// Запись свойства `interrupts-extended`.
    Interrupt,
}

/// Линия, выданная сессии.
pub struct IssuedLine<'a, L> {
    /// Направление линии.
    pub direction: Direction,
    /// Данные линии драйвера.
    pub line: &'a L,
}

/// Линии, выданные сессиям поставщика выводов, по значкам сессий. Номер линии - позиция записи
/// в порядке выдачи сессии, `L` - данные линии драйвера.
pub struct Lines<L> {
    sessions: BTreeMap<u64, Session<L>>,
}

struct Session<L> {
    node: DeviceId,
    lines: Vec<Issued<L>>,
}

struct Issued<L> {
    node: DeviceId,
    record: Key,
    index: u8,
    direction: Direction,
    line: L,
}

enum Key {
    Line(String),
    Interrupt,
}

impl<L> Lines<L> {
    /// Таблица без сессий.
    pub const fn new() -> Self {
        Self {
            sessions: BTreeMap::new(),
        }
    }

    /// Создаёт запись сессии со значком `badge` узла `node` без линий.
    pub fn open(&mut self, badge: u64, node: DeviceId) {
        self.sessions.insert(
            badge,
            Session {
                node,
                lines: Vec::new(),
            },
        );
    }

    /// Путь узла сессии со значком `badge` либо `None`, если у таблицы нет записи сессии.
    pub fn node(&self, badge: u64) -> Option<&DeviceId> {
        self.sessions.get(&badge).map(|session| &session.node)
    }

    /// Номер линии по записи `record` с индексом `index` узла `node`, выданной сессии `badge`
    /// направлением `direction`, либо `None`, если линия не выдана и сессии хватает номеров.
    ///
    /// Отказы:
    /// - [`PinsError::NotIssued`] - у таблицы нет записи сессии;
    /// - [`PinsError::Busy`] - линия выдана сессии другим направлением;
    /// - [`PinsError::NoSpace`] - сессии выдано 256 линий.
    pub fn find(
        &self,
        badge: u64,
        node: &DeviceId,
        record: Record<'_>,
        index: u8,
        direction: Direction,
    ) -> Result<Option<u8>, PinsError> {
        let session = self.sessions.get(&badge).ok_or(PinsError::NotIssued)?;
        let issued = session.lines.iter().enumerate().find(|(_, issued)| {
            issued.node == *node && issued.record.record() == record && issued.index == index
        });

        match issued {
            Some((number, issued)) if issued.direction == direction => u8::try_from(number)
                .map(Some)
                .map_err(|_| PinsError::NoSpace),
            Some(_) => Err(PinsError::Busy),
            None => u8::try_from(session.lines.len())
                .map(|_| None)
                .map_err(|_| PinsError::NoSpace),
        }
    }

    /// Выдаёт сессии `badge` следующий номер линии по записи `record` с индексом `index` узла
    /// `node` направлением `direction` с данными `line`. Повтор линии проверяет [`Lines::find`].
    ///
    /// Отказы:
    /// - [`PinsError::NotIssued`] - у таблицы нет записи сессии;
    /// - [`PinsError::NoSpace`] - сессии выдано 256 линий.
    pub fn issue(
        &mut self,
        badge: u64,
        node: DeviceId,
        record: Record<'_>,
        index: u8,
        direction: Direction,
        line: L,
    ) -> Result<u8, PinsError> {
        let session = self.sessions.get_mut(&badge).ok_or(PinsError::NotIssued)?;
        let number = u8::try_from(session.lines.len()).map_err(|_| PinsError::NoSpace)?;
        session.lines.push(Issued {
            node,
            record: Key::from(record),
            index,
            direction,
            line,
        });
        Ok(number)
    }

    /// Данные линии `line` сессии `badge`.
    ///
    /// Отказы:
    /// - [`PinsError::NotIssued`] - линия не выдана сессии.
    pub fn get(&self, badge: u64, line: u8) -> Result<&L, PinsError> {
        self.entry(badge, line).map(|issued| &issued.line)
    }

    /// Данные линии-входа `line` сессии `badge`.
    ///
    /// Отказы:
    /// - [`PinsError::NotIssued`] - линия не выдана сессии как вход.
    pub fn input(&self, badge: u64, line: u8) -> Result<&L, PinsError> {
        self.directed(badge, line, Direction::Input)
    }

    /// Данные линии-выхода `line` сессии `badge`.
    ///
    /// Отказы:
    /// - [`PinsError::NotIssued`] - линия не выдана сессии через `output`.
    pub fn output(&self, badge: u64, line: u8) -> Result<&L, PinsError> {
        self.directed(badge, line, Direction::Output)
    }

    /// Линии сессии `badge` в порядке выдачи.
    pub fn issued(&self, badge: u64) -> impl Iterator<Item = IssuedLine<'_, L>> {
        self.sessions
            .get(&badge)
            .into_iter()
            .flat_map(|session| session.lines.iter())
            .map(|issued| IssuedLine {
                direction: issued.direction,
                line: &issued.line,
            })
    }

    /// Изменяемые данные линии `line` сессии `badge`.
    ///
    /// Отказы:
    /// - [`PinsError::NotIssued`] - линия не выдана сессии.
    pub fn get_mut(&mut self, badge: u64, line: u8) -> Result<&mut L, PinsError> {
        self.sessions
            .get_mut(&badge)
            .and_then(|session| session.lines.get_mut(usize::from(line)))
            .map(|issued| &mut issued.line)
            .ok_or(PinsError::NotIssued)
    }

    /// Удаляет запись сессии `badge` вместе с данными её линий.
    pub fn release(&mut self, badge: u64) {
        self.sessions.remove(&badge);
    }

    fn entry(&self, badge: u64, line: u8) -> Result<&Issued<L>, PinsError> {
        self.sessions
            .get(&badge)
            .and_then(|session| session.lines.get(usize::from(line)))
            .ok_or(PinsError::NotIssued)
    }

    fn directed(&self, badge: u64, line: u8, direction: Direction) -> Result<&L, PinsError> {
        self.entry(badge, line)
            .ok()
            .filter(|issued| issued.direction == direction)
            .map(|issued| &issued.line)
            .ok_or(PinsError::NotIssued)
    }
}

impl<L> Default for Lines<L> {
    fn default() -> Self {
        Self::new()
    }
}

impl Key {
    fn record(&self) -> Record<'_> {
        match self {
            Self::Line(name) => Record::Line(name),
            Self::Interrupt => Record::Interrupt,
        }
    }
}

impl From<Record<'_>> for Key {
    fn from(record: Record<'_>) -> Self {
        match record {
            Record::Line(name) => Self::Line(name.into()),
            Record::Interrupt => Self::Interrupt,
        }
    }
}
