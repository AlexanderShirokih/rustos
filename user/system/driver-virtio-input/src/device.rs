//! Дескриптор устройства по конфигурации и перевод кадров событий в команды.

use alloc::{string::String, vec::Vec};

use input_abi::{
    AxisDecl, AxisList, Binding, Command, DeviceDescriptor, KeyList, RecordsError, Usage,
};
use virtio::{
    Registers, Transport,
    input::{self, AbsInfo, Event, SECTION_BYTES, kind, select, sync},
};

const FALLBACK_NAME: &str = "virtio-input";

const KEY_RELEASED: i32 = 0;
const KEY_PRESSED: i32 = 1;

/// Чтение разделов конфигурации virtio-input.
pub trait Config {
    /// Тип отказа чтения.
    type Error;

    /// Копирует байты раздела `select` с `subsel` в `destination` и возвращает их число.
    fn section(
        &mut self,
        select: u8,
        subsel: u8,
        destination: &mut [u8; SECTION_BYTES],
    ) -> Result<usize, Self::Error>;

    /// Читает диапазон абсолютной оси `axis`. `None` - конфигурация не содержит раздела оси.
    fn abs_info(&mut self, axis: u8) -> Result<Option<AbsInfo>, Self::Error>;
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

impl<R: Registers> Config for Transport<R> {
    type Error = R::Error;

    fn section(
        &mut self,
        select: u8,
        subsel: u8,
        destination: &mut [u8; SECTION_BYTES],
    ) -> Result<usize, R::Error> {
        input::read_section(self, select, subsel, destination).map(<[u8]>::len)
    }

    fn abs_info(&mut self, axis: u8) -> Result<Option<AbsInfo>, R::Error> {
        input::read_abs_info(self, axis)
    }
}

impl Device {
    /// Строит дескриптор по конфигурации.
    ///
    /// Имя берётся из `ID_NAME` до первого нулевого байта, имя не в UTF-8 заменяется на
    /// `virtio-input`. Оси - переведённые коды `EV_REL`, затем `EV_ABS` с диапазоном `ABS_INFO`,
    /// в пределе модели и без повтора usage. Клавиши - переведённые коды `EV_KEY` по
    /// возрастанию. Привязка - `Screen` при абсолютных X и Y.
    pub fn describe<C: Config>(config: &mut C) -> Result<Self, C::Error> {
        let mut section = [0; SECTION_BYTES];
        let length = config.section(select::ID_NAME, 0, &mut section)?;
        let name = section
            .get(..length)
            .and_then(|name| name.split(|byte| *byte == 0).next())
            .and_then(|name| core::str::from_utf8(name).ok())
            .unwrap_or(FALLBACK_NAME)
            .into();

        let mut axes = AxisList::new();
        let mut axis_states: Vec<AxisState> = Vec::new();
        let declared = |states: &[AxisState], usage| states.iter().any(|axis| axis.usage == usage);
        for code in codes(config, kind::REL, &mut section)? {
            if let Some(usage) = Usage::from_linux_rel(code.into())
                && !declared(&axis_states, usage)
                && axes.push(AxisDecl::relative(usage)).is_ok()
            {
                axis_states.push(AxisState::new(kind::REL, code, usage));
            }
        }
        for code in codes(config, kind::ABS, &mut section)? {
            let (Some(usage), Ok(subsel)) =
                (Usage::from_linux_abs(code.into()), u8::try_from(code))
            else {
                continue;
            };
            if !declared(&axis_states, usage)
                && let Some(AbsInfo { min, max }) = config.abs_info(subsel)?
                && axes.push(AxisDecl::absolute(usage, min, max)).is_ok()
            {
                axis_states.push(AxisState::new(kind::ABS, code, usage));
            }
        }

        let mut keys = KeyList::new();
        let mut key_states = Vec::new();
        for code in codes(config, kind::KEY, &mut section)? {
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
        let screen = absolute(Usage::X) && absolute(Usage::Y);
        Ok(Self {
            name,
            binding: if screen {
                Binding::Screen
            } else {
                Binding::None
            },
            keys,
            axes,
            contact_axes: AxisList::new(),
            key_states,
            axis_states,
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
            0,
            &self.contact_axes,
        )
    }

    /// Учитывает событие и по `SYN_REPORT` возвращает непустую команду кадра.
    ///
    /// Кадр хранит итоговое состояние: сумму относительной оси, последнее значение абсолютной
    /// и последнее состояние клавиши. Нажатие и отпускание клавиши внутри одного кадра
    /// команды не дают. Клавиша в текущем состоянии, повтор клавиши и событие вне дескриптора
    /// отбрасываются. `SYN_DROPPED` отбрасывает кадр до следующего `SYN_REPORT` включительно,
    /// который отпускает нажатые клавиши.
    ///
    /// Отказы:
    /// - `Full` - команда кадра не помещается в сообщение.
    pub fn translate(&mut self, event: Event) -> Result<Option<Command>, RecordsError> {
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
        Ok(None)
    }

    fn finish_frame(&mut self) -> Result<Option<Command>, RecordsError> {
        if core::mem::take(&mut self.dropping) {
            self.clear_frame();
            for key in &mut self.key_states {
                key.pending = Some(false);
            }
        }

        let mut command = Command::new();
        let mut records = 0;
        for (index, axis) in (0..).zip(&self.axis_states) {
            if let Some(value) = axis.pending {
                command.axis(index, value)?;
                records += 1;
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
                records += 1;
            }
        }
        self.clear_frame();
        Ok((records != 0).then_some(command))
    }

    fn clear_frame(&mut self) {
        for key in &mut self.key_states {
            key.pending = None;
        }
        for axis in &mut self.axis_states {
            axis.pending = None;
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

fn codes<C: Config>(
    config: &mut C,
    kind: u16,
    section: &mut [u8; SECTION_BYTES],
) -> Result<Vec<u16>, C::Error> {
    let subsel = u8::try_from(kind).unwrap_or(u8::MAX);
    let length = config.section(select::EV_BITS, subsel, section)?;
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
