//! Таблица контактов источника: занятость контактов и последние значения их осей.

use crate::{
    command::Command,
    descriptor::{AXES_MAX, CONTACTS_MAX},
    record::RecordsError,
};

const CONTACT_SLOTS: usize = CONTACTS_MAX as usize;

/// Контакты устройства источника и кадр их изменений.
///
/// Источник за кадр сообщает значения осей контактов и их касания, а [`ContactTable::finish`]
/// дописывает в команду записи, которые `Tracker` хаба принимает. Касание начинается после
/// значений всех осей контакта, у занятого контакта передаются только изменившиеся оси, а
/// начало или конец касания контакта встречается в команде не больше одного раза. Значения
/// осей сохраняются между кадрами и касаниями. Номер контакта или оси вне таблицы
/// игнорируется.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactTable {
    contacts: u16,
    axes: usize,
    slots: [Slot; CONTACT_SLOTS],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Slot {
    state: State,
    request: Request,
    known: [Option<i32>; AXES_MAX],
    pending: [Option<i32>; AXES_MAX],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Free,
    Awaiting,
    Busy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Request {
    None,
    Touch,
    Lift,
    Restart,
}

impl ContactTable {
    /// Свободные контакты `0..contacts` с осями `0..axes` без значений. Число контактов и осей
    /// ограничено пределами дескриптора [`CONTACTS_MAX`] и [`AXES_MAX`].
    pub fn new(contacts: u16, axes: u16) -> Self {
        Self {
            contacts: contacts.min(CONTACTS_MAX),
            axes: usize::from(axes).min(AXES_MAX),
            slots: [Slot {
                state: State::Free,
                request: Request::None,
                known: [None; AXES_MAX],
                pending: [None; AXES_MAX],
            }; CONTACT_SLOTS],
        }
    }

    /// Значение `value` оси `axis` контакта `contact` в текущем кадре.
    pub fn axis(&mut self, contact: u16, axis: u16, value: i32) {
        let declared = self.axes;
        if let Some(pending) = self
            .slot(contact)
            .and_then(|slot| slot.pending.get_mut(..declared))
            .and_then(|pending| pending.get_mut(usize::from(axis)))
        {
            *pending = Some(value);
        }
    }

    /// Касание контактом `contact`: свободный контакт начинает касание, как только известны
    /// значения всех его осей.
    pub fn touch(&mut self, contact: u16) {
        self.request(contact, Request::Touch);
    }

    /// Конец касания контактом `contact`.
    pub fn lift(&mut self, contact: u16) {
        self.request(contact, Request::Lift);
    }

    /// Новое касание контактом `contact`: занятый контакт заканчивает касание в команде кадра и
    /// начинает новое во второй команде, которую возвращает [`ContactTable::finish`].
    pub fn restart(&mut self, contact: u16) {
        self.request(contact, Request::Restart);
    }

    /// Дописывает в `command` изменения кадра и возвращает вторую команду с началом касаний
    /// после [`ContactTable::restart`] либо `None`.
    ///
    /// Отказы:
    /// - `Full` - записи не помещаются в команду.
    pub fn finish(&mut self, command: &mut Command) -> Result<Option<Command>, RecordsError> {
        let mut restarted = None;
        let axes = self.axes;
        for (contact, slot) in (0..self.contacts).zip(&mut self.slots) {
            let request = core::mem::replace(&mut slot.request, Request::None);
            let restart = request == Request::Restart && slot.state == State::Busy;
            slot.state = match (slot.state, request) {
                (state, Request::None) => state,
                (State::Busy, Request::Lift) => {
                    command.up(contact)?;
                    State::Free
                }
                (State::Busy, Request::Restart) => {
                    command.up(contact)?;
                    State::Awaiting
                }
                (State::Busy, Request::Touch) => State::Busy,
                (_, Request::Lift) => State::Free,
                (_, Request::Touch | Request::Restart) => State::Awaiting,
            };

            let busy = slot.state == State::Busy;
            for (axis, (known, pending)) in
                (0..).zip(slot.known.iter_mut().zip(&mut slot.pending).take(axes))
            {
                if let Some(value) = pending.take()
                    && *known != Some(value)
                {
                    if busy {
                        command.contact_axis(contact, axis, value.into())?;
                    }
                    *known = Some(value);
                }
            }

            let known = slot.known.get(..axes).unwrap_or_default();
            if slot.state == State::Awaiting && known.iter().all(Option::is_some) {
                let target = if restart {
                    restarted.get_or_insert_with(Command::new)
                } else {
                    &mut *command
                };
                target.down(contact)?;
                for (axis, value) in (0..).zip(known.iter().flatten()) {
                    target.contact_axis(contact, axis, (*value).into())?;
                }
                slot.state = State::Busy;
            }
        }
        Ok(restarted)
    }

    /// Дописывает в `command` отмену каждого занятого касания, освобождает контакты и
    /// отбрасывает кадр. Значения осей сохраняются.
    ///
    /// Отказы:
    /// - `Full` - записи не помещаются в команду.
    pub fn cancel(&mut self, command: &mut Command) -> Result<(), RecordsError> {
        for (contact, slot) in (0..self.contacts).zip(&mut self.slots) {
            if slot.state == State::Busy {
                command.cancel(contact)?;
            }
            slot.state = State::Free;
            slot.request = Request::None;
            slot.pending = [None; AXES_MAX];
        }
        Ok(())
    }

    fn slot(&mut self, contact: u16) -> Option<&mut Slot> {
        if contact >= self.contacts {
            return None;
        }
        self.slots.get_mut(usize::from(contact))
    }

    fn request(&mut self, contact: u16, request: Request) {
        if let Some(slot) = self.slot(contact) {
            slot.request = request;
        }
    }
}
