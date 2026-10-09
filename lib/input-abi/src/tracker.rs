//! Состояние устройства ввода и применение команды к нему.

use crate::{
    command::COMMAND_RECORDS_MAX,
    descriptor::{AXES_MAX, AxisDecl, AxisMode, CONTACTS_MAX, DeviceLayout, KEYS_MAX},
    record::{CANCEL, Kind, NO_CONTACT, RECORD_SIZE, Record, RecordBuf, Records},
};

const CONTACT_SLOTS: usize = CONTACTS_MAX as usize;

const KEY_WORDS: usize = KEYS_MAX / 64;

/// Состояние одного устройства ввода: нажатые клавиши, значения абсолютных осей устройства,
/// занятые контакты со значениями их осей и время последнего кадра.
///
/// Трекер сверяет команду с дескриптором и состоянием, выдаёт кадр команды, снимок и
/// отмену состояния. Время приёма команды задаёт вызывающий.
#[derive(Debug, Clone)]
pub struct Tracker {
    device: u16,
    layout: DeviceLayout,
    state: State,
}

/// Отказ команды. Состояние устройства после отказа не меняется.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandError {
    /// Команда закодирована неверно:
    /// - длина записей не кратна [`RECORD_SIZE`];
    /// - команда без записей либо с записями больше [`COMMAND_RECORDS_MAX`];
    /// - запись вида вне `Key`, `Axis`, `ContactDown` и `ContactUp`;
    /// - ненулевое поле устройства;
    /// - флаг, который вид записи не определяет;
    /// - значение `Key` не 0 и не 1.
    Malformed,
    /// Запись выходит за дескриптор:
    /// - номер контакта не меньше `max_contacts` дескриптора;
    /// - индекс оси вне списка осей устройства или контакта, к которому относится запись;
    /// - значение абсолютной оси вне её диапазона;
    /// - индекс клавиши вне списка клавиш дескриптора.
    Undeclared,
    /// Запись противоречит состоянию устройства или другой записи команды:
    /// - `ContactDown` на занятом номере, `ContactUp` или `Axis` контакта на свободном;
    /// - второй `ContactDown` или `ContactUp` одного номера в команде;
    /// - `ContactDown` без значения какой-либо оси контакта в той же команде;
    /// - повтор оси одного устройства или контакта либо повтор клавиши в команде;
    /// - нажатие нажатой либо отпускание отпущенной клавиши;
    /// - время меньше времени предыдущего кадра.
    Conflict,
}

#[derive(Debug, Clone, Copy)]
struct State {
    pressed: KeyBits,
    device_values: [Option<i32>; AXES_MAX],
    contacts: [Contact; CONTACT_SLOTS],
    last_frame: u64,
}

#[derive(Debug, Clone, Copy)]
struct Contact {
    busy: bool,
    values: [i32; AXES_MAX],
}

#[derive(Debug, Clone, Copy, Default)]
struct KeyBits {
    words: [u64; KEY_WORDS],
}

#[derive(Default)]
struct SeenInCommand {
    keys: KeyBits,
    device_axes: [bool; AXES_MAX],
    contacts: [bool; CONTACT_SLOTS],
    started: [bool; CONTACT_SLOTS],
    contact_axes: [[bool; AXES_MAX]; CONTACT_SLOTS],
}

impl Tracker {
    /// Трекер устройства `device` без нажатых клавиш, значений осей и занятых контактов.
    pub fn new(device: u16, layout: DeviceLayout) -> Self {
        Self {
            device,
            layout,
            state: State {
                pressed: KeyBits::default(),
                device_values: [None; AXES_MAX],
                contacts: [Contact {
                    busy: false,
                    values: [0; AXES_MAX],
                }; CONTACT_SLOTS],
                last_frame: 0,
            },
        }
    }

    /// Применяет команду `command` со временем `time` и заменяет содержимое `out` её кадром.
    /// Команда без времени получает большее из `now` и времени предыдущего кадра.
    ///
    /// Отказы - варианты [`CommandError`]. При отказе состояние и `out` не меняются.
    pub fn apply(
        &mut self,
        command: &[u8],
        time: Option<u64>,
        now: u64,
        out: &mut RecordBuf,
    ) -> Result<(), CommandError> {
        let records = Records::read(command).map_err(|_| CommandError::Malformed)?;
        let count = command.len() / RECORD_SIZE;

        if count == 0 {
            return Err(CommandError::Malformed);
        }

        if count > COMMAND_RECORDS_MAX {
            return Err(CommandError::Malformed);
        }

        let frame_time = match time {
            Some(time) if time < self.state.last_frame => return Err(CommandError::Conflict),
            Some(time) => time,
            None => self.frame_time(now),
        };

        let mut next = self.state;
        let mut seen = SeenInCommand::default();
        for record in records {
            self.step(&mut next, &mut seen, &record)?;
        }

        let started = seen.started.iter().zip(&seen.contact_axes);
        let axes = self.layout.contact_axes.len();
        if started
            .filter(|(started, _)| **started)
            .any(|(_, set)| set.iter().take(axes).any(|seen| !seen))
        {
            return Err(CommandError::Conflict);
        }

        next.last_frame = frame_time;
        self.state = next;
        out.clear();
        for record in Records::read(command).into_iter().flatten() {
            emit(out, self.normalized(&record));
        }
        emit(out, Record::frame(self.device, frame_time));
        Ok(())
    }

    /// Заменяет содержимое `out` снимком: `Key` каждой нажатой клавиши, `Axis` каждой
    /// абсолютной оси устройства со значением, `ContactDown` и `Axis` осей каждого занятого
    /// контакта и `Frame` со временем последнего кадра.
    pub fn snapshot(&self, out: &mut RecordBuf) {
        out.clear();
        for key in self.state.pressed.iter() {
            emit(out, Record::key(self.device, key, true));
        }

        let values = (0..)
            .zip(self.state.device_values)
            .take(self.layout.device_axes.len());
        for (index, value) in values {
            if let Some(value) = value {
                let record = Record::axis(self.device, index, NO_CONTACT, i64::from(value));
                emit(out, record);
            }
        }

        for (number, contact) in self.contacts() {
            if !contact.busy {
                continue;
            }
            emit(out, Record::contact_down(self.device, number));
            let values = (0..)
                .zip(contact.values)
                .take(self.layout.contact_axes.len());
            for (index, value) in values {
                let record = Record::axis(self.device, index, number, i64::from(value));
                emit(out, record);
            }
        }

        emit(out, Record::frame(self.device, self.state.last_frame));
    }

    /// Освобождает контакты и отпускает клавиши. Заменяет содержимое `out` кадром отмены:
    /// `ContactUp` с [`CANCEL`] каждого занятого контакта, `Key` со значением 0 каждой нажатой
    /// клавиши и `Frame` со временем по правилу команды без времени. Без занятых контактов и
    /// нажатых клавиш `out` остаётся пустым. Значения абсолютных осей устройства сохраняются.
    pub fn cancel(&mut self, now: u64, out: &mut RecordBuf) {
        out.clear();
        let busy = self.state.contacts.iter().any(|contact| contact.busy);
        if !busy && self.state.pressed.is_empty() {
            return;
        }

        for (number, contact) in self.contacts() {
            if contact.busy {
                emit(out, Record::contact_up(self.device, number, true));
            }
        }
        for key in self.state.pressed.iter() {
            emit(out, Record::key(self.device, key, false));
        }

        let time = self.frame_time(now);
        emit(out, Record::frame(self.device, time));
        for contact in &mut self.state.contacts {
            contact.busy = false;
        }
        self.state.pressed = KeyBits::default();
        self.state.last_frame = time;
    }

    fn step(
        &self,
        next: &mut State,
        seen: &mut SeenInCommand,
        record: &Record,
    ) -> Result<(), CommandError> {
        if record.device != 0 {
            return Err(CommandError::Malformed);
        }

        let allowed_flags = if record.kind == Kind::ContactUp {
            CANCEL
        } else {
            0
        };
        if record.flags & !allowed_flags != 0 {
            return Err(CommandError::Malformed);
        }

        match record.kind {
            Kind::Key => self.step_key(next, seen, record),
            Kind::Axis if record.contact == NO_CONTACT => self.step_device_axis(next, seen, record),
            Kind::Axis => self.step_contact_axis(next, seen, record),
            Kind::ContactDown | Kind::ContactUp => self.step_contact(next, seen, record),
            Kind::Frame
            | Kind::DeviceAdded
            | Kind::DeviceRemoved
            | Kind::Resync
            | Kind::Unknown(_) => Err(CommandError::Malformed),
        }
    }

    fn step_key(
        &self,
        next: &mut State,
        seen: &mut SeenInCommand,
        record: &Record,
    ) -> Result<(), CommandError> {
        let key = record.code;
        if self.layout.key(key).is_none() {
            return Err(CommandError::Undeclared);
        }

        let pressed = match record.value {
            0 => false,
            1 => true,
            _ => return Err(CommandError::Malformed),
        };

        if seen.keys.contains(key) {
            return Err(CommandError::Conflict);
        }
        seen.keys.set(key, true);

        if next.pressed.contains(key) == pressed {
            return Err(CommandError::Conflict);
        }
        next.pressed.set(key, pressed);
        Ok(())
    }

    fn step_device_axis(
        &self,
        next: &mut State,
        seen: &mut SeenInCommand,
        record: &Record,
    ) -> Result<(), CommandError> {
        let index = usize::from(record.code);
        let decl = self
            .layout
            .device_axis(record.code)
            .ok_or(CommandError::Undeclared)?;
        mark_first(seen.device_axes.get_mut(index), CommandError::Conflict)?;

        if let AxisMode::Absolute { .. } = decl.mode {
            let value = in_range(decl, record.value)?;
            if let Some(slot) = next.device_values.get_mut(index) {
                *slot = Some(value);
            }
        }
        Ok(())
    }

    fn step_contact_axis(
        &self,
        next: &mut State,
        seen: &mut SeenInCommand,
        record: &Record,
    ) -> Result<(), CommandError> {
        let number = self.contact_index(record.contact)?;
        let contact = next
            .contacts
            .get_mut(number)
            .ok_or(CommandError::Undeclared)?;
        if !contact.busy {
            return Err(CommandError::Conflict);
        }

        let index = usize::from(record.code);
        let decl = self
            .layout
            .contact_axis(record.code)
            .ok_or(CommandError::Undeclared)?;
        let axes = seen
            .contact_axes
            .get_mut(number)
            .ok_or(CommandError::Undeclared)?;
        mark_first(axes.get_mut(index), CommandError::Conflict)?;

        let value = in_range(decl, record.value)?;
        if let Some(slot) = contact.values.get_mut(index) {
            *slot = value;
        }
        Ok(())
    }

    fn step_contact(
        &self,
        next: &mut State,
        seen: &mut SeenInCommand,
        record: &Record,
    ) -> Result<(), CommandError> {
        let number = self.contact_index(record.contact)?;
        mark_first(seen.contacts.get_mut(number), CommandError::Conflict)?;

        let contact = next
            .contacts
            .get_mut(number)
            .ok_or(CommandError::Undeclared)?;
        let down = record.kind == Kind::ContactDown;
        if contact.busy == down {
            return Err(CommandError::Conflict);
        }

        contact.busy = down;

        if down && let Some(started) = seen.started.get_mut(number) {
            *started = true;
        }
        Ok(())
    }

    fn contact_index(&self, contact: u16) -> Result<usize, CommandError> {
        if contact < self.layout.max_contacts {
            Ok(usize::from(contact))
        } else {
            Err(CommandError::Undeclared)
        }
    }

    fn contacts(&self) -> impl Iterator<Item = (u16, Contact)> + '_ {
        (0..self.layout.max_contacts).zip(self.state.contacts)
    }

    fn frame_time(&self, now: u64) -> u64 {
        now.max(self.state.last_frame)
    }

    fn normalized(&self, record: &Record) -> Record {
        match record.kind {
            Kind::Key => Record::key(self.device, record.code, record.value == 1),
            Kind::Axis => Record::axis(self.device, record.code, record.contact, record.value),
            Kind::ContactDown => Record::contact_down(self.device, record.contact),
            _ => Record::contact_up(self.device, record.contact, record.flags & CANCEL != 0),
        }
    }
}

impl KeyBits {
    fn contains(&self, key: u16) -> bool {
        let (word, mask) = Self::position(key);
        self.words.get(word).is_some_and(|bits| bits & mask != 0)
    }

    fn set(&mut self, key: u16, present: bool) {
        let (word, mask) = Self::position(key);
        if let Some(bits) = self.words.get_mut(word) {
            if present {
                *bits |= mask;
            } else {
                *bits &= !mask;
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.words.iter().all(|&bits| bits == 0)
    }

    fn iter(&self) -> impl Iterator<Item = u16> + '_ {
        (0..=u16::MAX)
            .take(KEYS_MAX)
            .filter(|&key| self.contains(key))
    }

    fn position(key: u16) -> (usize, u64) {
        (usize::from(key / 64), 1 << (key % 64))
    }
}

fn mark_first(slot: Option<&mut bool>, repeated: CommandError) -> Result<(), CommandError> {
    match slot {
        Some(seen) if !*seen => {
            *seen = true;
            Ok(())
        }
        _ => Err(repeated),
    }
}

fn emit(out: &mut RecordBuf, record: Record) {
    // Кадр команды ограничен `COMMAND_RECORDS_MAX`, снимок и отмена - наибольшим снимком,
    // который проверка дескриптора держит в пределах буфера.
    let _ = out.push(record);
}

fn in_range(decl: AxisDecl, value: i64) -> Result<i32, CommandError> {
    let AxisMode::Absolute { min, max } = decl.mode else {
        return Err(CommandError::Undeclared);
    };
    if value < i64::from(min) || value > i64::from(max) {
        return Err(CommandError::Undeclared);
    }
    i32::try_from(value).map_err(|_| CommandError::Undeclared)
}
