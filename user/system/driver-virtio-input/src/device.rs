//! Дескриптор устройства по конфигурации и перевод кадров событий в команды.

use alloc::{string::String, vec::Vec};

use input_abi::{
    AxisDecl, AxisList, Binding, CONTACTS_MAX, Command, ContactTable, DeviceDescriptor, KeyList,
    RecordsError, Usage,
};
use virtio::{
    Registers, Transport,
    input::{self, AbsInfo, Event, SECTION_BYTES, abs, kind, select, sync},
};

const FALLBACK_NAME: &str = "virtio-input";

const KEY_RELEASED: i32 = 0;
const KEY_PRESSED: i32 = 1;

/// Чтение разделов конфигурации virtio-input.
#[allow(async_fn_in_trait)]
pub trait Config {
    /// Тип отказа чтения.
    type Error;

    /// Копирует байты раздела `select` с `subsel` в `destination` и возвращает их число.
    async fn section(
        &mut self,
        select: u8,
        subsel: u8,
        destination: &mut [u8; SECTION_BYTES],
    ) -> Result<usize, Self::Error>;

    /// Читает диапазон абсолютной оси `axis`. `None` - конфигурация не содержит раздела оси.
    async fn abs_info(&mut self, axis: u8) -> Result<Option<AbsInfo>, Self::Error>;
}

/// Устройство ввода: дескриптор, коды событий его клавиш и осей и состояние кадра.
pub struct Device {
    name: String,
    binding: Binding,
    keys: KeyList,
    axes: AxisList,
    contact_axes: AxisList,
    key_states: Vec<KeyState>,
    axis_states: Vec<AxisState>,
    contact_codes: Vec<u16>,
    contacts: ContactTable,
    slots: Vec<Slot>,
    slot: usize,
    dropping: bool,
}

struct KeyState {
    code: u16,
    pressed: bool,
    pending: Option<bool>,
}

struct AxisState {
    kind: u16,
    code: u16,
    usage: Usage,
    pending: Option<i64>,
}

#[derive(Clone, Copy)]
struct Slot {
    touch: Option<i32>,
    pending_touch: Option<i32>,
}

impl<R: Registers> Config for Transport<R> {
    type Error = R::Error;

    async fn section(
        &mut self,
        select: u8,
        subsel: u8,
        destination: &mut [u8; SECTION_BYTES],
    ) -> Result<usize, R::Error> {
        input::read_section(self, select, subsel, destination)
            .await
            .map(<[u8]>::len)
    }

    async fn abs_info(&mut self, axis: u8) -> Result<Option<AbsInfo>, R::Error> {
        input::read_abs_info(self, axis).await
    }
}

impl Device {
    /// Строит дескриптор по конфигурации.
    ///
    /// Имя берётся из `ID_NAME` до первого нулевого байта, имя не в UTF-8 заменяется на
    /// `virtio-input`. Оси - переведённые коды `EV_REL`, затем `EV_ABS` с диапазоном
    /// `ABS_INFO`, в пределе модели и без повтора usage. Устройство с [`abs::MT_SLOT`]
    /// объявляет контакты по диапазону слотов в пределе модели, оси контакта X и Y по
    /// [`abs::MT_POSITION_X`] и [`abs::MT_POSITION_Y`] и не объявляет абсолютных осей
    /// устройства. Клавиши - переведённые коды `EV_KEY` по возрастанию. Привязка - `Screen`
    /// при абсолютных X и Y устройства либо контакта.
    pub async fn describe<C: Config>(config: &mut C) -> Result<Self, C::Error> {
        let mut section = [0; SECTION_BYTES];
        let name = name(config, &mut section).await?;

        let mut axes = AxisList::new();
        let mut axis_states: Vec<AxisState> = Vec::new();
        let declared = |states: &[AxisState], usage| states.iter().any(|axis| axis.usage == usage);
        for code in codes(config, kind::REL, &mut section).await? {
            if let Some(usage) = Usage::from_linux_rel(code.into())
                && !declared(&axis_states, usage)
                && axes.push(AxisDecl::relative(usage)).is_ok()
            {
                axis_states.push(AxisState::new(kind::REL, code, usage));
            }
        }

        let abs_codes = codes(config, kind::ABS, &mut section).await?;
        let multitouch = abs_codes.contains(&abs::MT_SLOT);
        let mut contacts = if multitouch {
            slot_count(config).await?
        } else {
            0
        };

        let mut contact_axes = AxisList::new();
        let mut contact_usages = Vec::new();
        let mut contact_codes = Vec::new();
        for code in abs_codes {
            let Ok(subsel) = u8::try_from(code) else {
                continue;
            };
            let contact_usage = match code {
                abs::MT_POSITION_X => Some(Usage::X),
                abs::MT_POSITION_Y => Some(Usage::Y),
                _ => None,
            };

            if let Some(usage) = contact_usage {
                if contacts != 0
                    && let Some(AbsInfo { min, max }) = config.abs_info(subsel).await?
                    && contact_axes
                        .push(AxisDecl::absolute(usage, min, max))
                        .is_ok()
                {
                    contact_usages.push(usage);
                    contact_codes.push(code);
                }
            } else if !multitouch
                && let Some(usage) = Usage::from_linux_abs(code.into())
                && !declared(&axis_states, usage)
                && let Some(AbsInfo { min, max }) = config.abs_info(subsel).await?
                && axes.push(AxisDecl::absolute(usage, min, max)).is_ok()
            {
                axis_states.push(AxisState::new(kind::ABS, code, usage));
            }
        }
        if contact_codes.is_empty() {
            contacts = 0;
        }

        let mut keys = KeyList::new();
        let mut key_states = Vec::new();
        for code in codes(config, kind::KEY, &mut section).await? {
            if let Some(usage) = Usage::from_linux_key(code.into())
                && keys.push(usage).is_ok()
            {
                key_states.push(KeyState {
                    code,
                    pressed: false,
                    pending: None,
                });
            }
        }

        let absolute = |usage| {
            axis_states
                .iter()
                .any(|axis| axis.kind == kind::ABS && axis.usage == usage)
        };
        let screen = absolute(Usage::X) && absolute(Usage::Y)
            || contact_usages.contains(&Usage::X) && contact_usages.contains(&Usage::Y);
        let contact_table = ContactTable::new(
            u16::try_from(contacts).unwrap_or(CONTACTS_MAX),
            u16::try_from(contact_codes.len()).unwrap_or(u16::MAX),
        );
        Ok(Self {
            name,
            binding: if screen {
                Binding::Screen
            } else {
                Binding::None
            },
            keys,
            axes,
            contact_axes,
            key_states,
            axis_states,
            contact_codes,
            contacts: contact_table,
            slots: alloc::vec![Slot::FREE; contacts],
            slot: 0,
            dropping: false,
        })
    }

    /// Имя устройства.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Дескриптор устройства. `None`, если имя длиннее предела модели.
    pub fn descriptor(&self) -> Option<DeviceDescriptor<'_>> {
        DeviceDescriptor::new(
            &self.name,
            self.binding,
            &self.keys,
            &self.axes,
            self.contact_count(),
            &self.contact_axes,
        )
    }

    /// Учитывает событие и по `SYN_REPORT` возвращает команды кадра по порядку.
    ///
    /// Кадр хранит итоговое состояние: сумму относительной оси, последнее значение абсолютной
    /// и последнее состояние клавиши. Нажатие и отпускание клавиши внутри одного кадра
    /// команды не дают. Клавиша в текущем состоянии, повтор клавиши и событие вне дескриптора
    /// отбрасываются. `SYN_DROPPED` отбрасывает кадр до следующего `SYN_REPORT` включительно,
    /// который отпускает нажатые клавиши и отменяет касания.
    ///
    /// Слот выбирает [`abs::MT_SLOT`] и сохраняет его до следующего `MT_SLOT`. Номер касания
    /// не меньше нуля на свободном слоте начинает касание со значениями всех осей контакта,
    /// когда каждое из них известно. Координата занятого слота даёт новое значение оси
    /// контакта, номер -1 заканчивает касание. Новый номер на занятом слоте заканчивает
    /// касание в первой команде кадра и начинает во второй.
    ///
    /// Отказы:
    /// - `Full` - команда кадра не помещается в сообщение.
    pub fn translate(&mut self, event: Event) -> Result<Vec<Command>, RecordsError> {
        match (event.kind, event.code) {
            (kind::SYN, sync::REPORT) => return self.finish_frame(),
            (kind::SYN, sync::DROPPED) => {
                self.dropping = true;
                self.clear_frame();
            }
            _ if self.dropping => {}
            (kind::KEY, code) => {
                let key = self.key_states.iter_mut().find(|key| key.code == code);
                let state = match event.value {
                    KEY_PRESSED => Some(true),
                    KEY_RELEASED => Some(false),
                    _ => None,
                };
                if let (Some(key), Some(state)) = (key, state) {
                    key.pending = Some(state);
                }
            }
            (kind::ABS, code) if !self.slots.is_empty() => self.contact_event(code, event.value),
            (kind @ (kind::REL | kind::ABS), code) => {
                let axis = self
                    .axis_states
                    .iter_mut()
                    .find(|axis| axis.kind == kind && axis.code == code);
                if let Some(axis) = axis {
                    let value = i64::from(event.value);
                    axis.pending = match (kind, axis.pending) {
                        (kind::REL, Some(sum)) => Some(sum.saturating_add(value)),
                        _ => Some(value),
                    };
                }
            }
            _ => {}
        }
        Ok(Vec::new())
    }

    fn contact_count(&self) -> u16 {
        u16::try_from(self.slots.len()).unwrap_or(CONTACTS_MAX)
    }

    fn contact_event(&mut self, code: u16, value: i32) {
        match code {
            abs::MT_SLOT => self.slot = usize::try_from(value).unwrap_or(usize::MAX),
            abs::MT_TRACKING_ID => {
                if let Some(slot) = self.slots.get_mut(self.slot) {
                    slot.pending_touch = Some(value);
                }
            }
            _ => {
                let axis = self.contact_codes.iter().position(|known| *known == code);
                let contact = u16::try_from(self.slot).ok();
                if let Some((contact, axis)) = contact.zip(axis.and_then(|a| u16::try_from(a).ok()))
                {
                    self.contacts.axis(contact, axis, value);
                }
            }
        }
    }

    fn finish_frame(&mut self) -> Result<Vec<Command>, RecordsError> {
        let dropped = core::mem::take(&mut self.dropping);
        if dropped {
            self.clear_frame();
            for key in &mut self.key_states {
                key.pending = Some(false);
            }
        }

        let mut command = Command::new();
        let mut restarted = None;
        for (index, axis) in (0..).zip(&self.axis_states) {
            if let Some(value) = axis.pending {
                command.axis(index, value)?;
            }
        }
        for (index, key) in (0..).zip(&mut self.key_states) {
            if let Some(state) = key.pending
                && state != key.pressed
            {
                if state {
                    command.press(index)?;
                } else {
                    command.release(index)?;
                }
                key.pressed = state;
            }
        }
        if dropped {
            self.contacts.cancel(&mut command)?;
            for slot in &mut self.slots {
                slot.touch = None;
            }
        } else {
            for (contact, slot) in (0..).zip(&mut self.slots) {
                slot.settle(contact, &mut self.contacts);
            }
            restarted = self.contacts.finish(&mut command)?;
        }
        self.clear_frame();
        let mut commands = Vec::new();
        if !command.as_bytes().is_empty() {
            commands.push(command);
        }
        commands.extend(restarted);
        Ok(commands)
    }

    fn clear_frame(&mut self) {
        for key in &mut self.key_states {
            key.pending = None;
        }
        for axis in &mut self.axis_states {
            axis.pending = None;
        }
        for slot in &mut self.slots {
            slot.pending_touch = None;
        }
    }
}

impl AxisState {
    fn new(kind: u16, code: u16, usage: Usage) -> Self {
        Self {
            kind,
            code,
            usage,
            pending: None,
        }
    }
}

impl Slot {
    const FREE: Self = Self {
        touch: None,
        pending_touch: None,
    };

    fn settle(&mut self, contact: u16, contacts: &mut ContactTable) {
        let Some(id) = self.pending_touch else {
            return;
        };
        if id < 0 {
            contacts.lift(contact);
            self.touch = None;
            return;
        }
        match self.touch {
            Some(current) if current == id => contacts.touch(contact),
            Some(_) => contacts.restart(contact),
            None => contacts.touch(contact),
        }

        self.touch = Some(id);
    }
}

async fn slot_count<C: Config>(config: &mut C) -> Result<usize, C::Error> {
    let Ok(subsel) = u8::try_from(abs::MT_SLOT) else {
        return Ok(0);
    };

    let slots = config
        .abs_info(subsel)
        .await?
        .map_or(0, |AbsInfo { max, .. }| {
            usize::try_from(max.saturating_add(1)).unwrap_or(0)
        });
    Ok(slots.min(usize::from(CONTACTS_MAX)))
}

async fn codes<C: Config>(
    config: &mut C,
    kind: u16,
    section: &mut [u8; SECTION_BYTES],
) -> Result<Vec<u16>, C::Error> {
    let subsel = u8::try_from(kind).unwrap_or(u8::MAX);
    let length = config.section(select::EV_BITS, subsel, section).await?;
    let bytes = section.get(..length).unwrap_or_default();
    Ok((0u16..)
        .zip(bytes)
        .flat_map(|(position, byte)| {
            (0..8)
                .filter(move |bit| byte >> bit & 1 != 0)
                .map(move |bit| position * 8 + bit)
        })
        .collect())
}

async fn name<C: Config>(
    config: &mut C,
    section: &mut [u8; SECTION_BYTES],
) -> Result<String, C::Error> {
    let length = config.section(select::ID_NAME, 0, section).await?;
    Ok(section
        .get(..length)
        .and_then(|name| name.split(|byte| *byte == 0).next())
        .and_then(|name| core::str::from_utf8(name).ok())
        .unwrap_or(FALLBACK_NAME)
        .into())
}
