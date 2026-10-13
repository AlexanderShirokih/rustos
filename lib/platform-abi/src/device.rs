use alloc::string::String;

use ipc::{
    IntoWireValue, ReceivedResources, WireTyped, WireValue,
    schema::WireType,
    wire::{IpcError, MessageBuf, value},
};

/// Предельная длина [`DeviceId`] в байтах.
pub const DEVICE_ID_MAX: usize = 255;

const SEPARATOR: char = '/';

/// Идентичность устройства: абсолютный путь его узла в описании платформы вместе с
/// unit-address. Путь начинается с `/`, его компоненты непусты, корень устройством не
/// считается.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DeviceId(String);

impl DeviceId {
    /// Проверяет путь `path` и копирует его. Путь без ведущего `/`, корень, путь с пустым
    /// компонентом и путь длиннее [`DEVICE_ID_MAX`] байт дают `None`.
    #[must_use]
    pub fn new(path: &str) -> Option<Self> {
        let names = path.strip_prefix(SEPARATOR)?;
        if path.len() > DEVICE_ID_MAX || names.split(SEPARATOR).any(str::is_empty) {
            return None;
        }

        Some(Self(String::from(path)))
    }

    /// Путь целиком, включая ведущий разделитель.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) const EMPTY: Self = Self(String::new());

    pub(crate) fn push(&mut self, name: &str) -> Option<()> {
        let len = self
            .0
            .len()
            .checked_add(SEPARATOR.len_utf8())?
            .checked_add(name.len())?;
        if len > DEVICE_ID_MAX {
            return None;
        }

        self.0.push(SEPARATOR);
        self.0.push_str(name);
        Some(())
    }

    pub(crate) fn pop(&mut self) {
        let len = self.0.rfind(SEPARATOR).unwrap_or_default();
        self.0.truncate(len);
    }
}

impl<'a, R: ReceivedResources> WireValue<'a, R> for DeviceId {
    type Owned = Self;

    fn from_field(data: &'a [u8], _frame: &mut R) -> Result<Self, IpcError> {
        Self::new(value::decode_str(data)?).ok_or(IpcError::InvalidValue)
    }
}

impl<R: ReceivedResources> IntoWireValue<R> for DeviceId {
    type Read = Self;

    fn write_into(self, frame: &mut MessageBuf<R>, field_id: u8) -> Result<(), IpcError> {
        frame.write_field(field_id, self.as_str().as_bytes())
    }
}

impl WireTyped for DeviceId {
    const WIRE_TYPE: WireType = WireType::BoundedStr(DEVICE_ID_MAX);
}
