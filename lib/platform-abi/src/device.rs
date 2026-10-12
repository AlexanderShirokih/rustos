use core::{cmp::Ordering, fmt};

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
#[derive(Clone)]
pub struct DeviceId {
    bytes: [u8; DEVICE_ID_MAX],
    len: usize,
}

impl DeviceId {
    /// Проверяет путь `path` и копирует его. Путь без ведущего `/`, корень, путь с пустым
    /// компонентом и путь длиннее [`DEVICE_ID_MAX`] байт дают `None`.
    #[must_use]
    pub fn new(path: &str) -> Option<Self> {
        let names = path.strip_prefix(SEPARATOR)?;
        if names.split(SEPARATOR).any(str::is_empty) {
            return None;
        }

        let mut id = Self::EMPTY;
        id.append(path)?;
        Some(id)
    }

    /// Путь целиком, включая ведущий разделитель.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.bytes
            .get(..self.len)
            .and_then(|bytes| core::str::from_utf8(bytes).ok())
            .unwrap_or_default()
    }

    pub(crate) const EMPTY: Self = Self {
        bytes: [0; DEVICE_ID_MAX],
        len: 0,
    };

    pub(crate) fn push(&mut self, name: &str) -> Option<()> {
        let len = self.len;
        let pushed = self.append("/").and_then(|()| self.append(name));
        if pushed.is_none() {
            self.len = len;
        }
        pushed
    }

    pub(crate) fn pop(&mut self) {
        self.len = self.as_str().rfind(SEPARATOR).unwrap_or_default();
    }

    fn append(&mut self, text: &str) -> Option<()> {
        let end = self.len.checked_add(text.len())?;
        self.bytes
            .get_mut(self.len..end)?
            .copy_from_slice(text.as_bytes());
        self.len = end;
        Some(())
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
    const WIRE_TYPE: WireType = WireType::Str;
}

impl fmt::Debug for DeviceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("DeviceId")
            .field(&self.as_str())
            .finish()
    }
}

impl PartialEq for DeviceId {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for DeviceId {}

impl PartialOrd for DeviceId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DeviceId {
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_str().cmp(other.as_str())
    }
}
