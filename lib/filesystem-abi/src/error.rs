//! Доменная ошибка контракта: закрытый набор исходов, в кадре - дискриминант `u16` LE.

use ipc::{
    WireTyped, WireValue,
    schema::WireType,
    wire::{ENUM_DISCRIMINANT_SIZE, HandleSlots, IpcError, MessageBuf, value},
};

/// Причина отказа файловой операции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileSystemError {
    /// Узла по указанному пути нет.
    NotFound,
    /// Компонент пути не является каталогом.
    NotADirectory,
    /// Операция над файлом применена к каталогу.
    IsADirectory,
    /// Каталог с записями, а операция требует пустого.
    NotEmpty,
    /// Узел уже существует, а открытие требует его отсутствия.
    AlreadyExists,
    /// Путь вне границ либо нарушает форму.
    InvalidPath,
    /// Дескриптор не открыт в этой сессии.
    BadDescriptor,
    /// Исчерпан потолок дескрипторов сессии.
    TooManyOpenFiles,
    /// Бекенд не допускает изменений.
    ReadOnly,
    /// Не хватает ресурса под запрошенный объём.
    NoSpace,
    /// Операция пересекает границу точки монтирования.
    CrossMount,
    /// Кадр пришёл по хэндлу без значка.
    NoSession,
    /// Буфер данных сессии не выделен.
    NoBuffer,
    /// Запрошенный объём превышает буфер сессии.
    BufferTooSmall,
    /// Бекенд не поддерживает операцию.
    NotSupported,
    /// Файловая система за монтированием не ответила в отведённый срок.
    Unresponsive,
    /// Операция вне режима, в котором открыт дескриптор.
    WrongMode,
}

const VARIANTS: [FileSystemError; 17] = [
    FileSystemError::NotFound,
    FileSystemError::NotADirectory,
    FileSystemError::IsADirectory,
    FileSystemError::NotEmpty,
    FileSystemError::AlreadyExists,
    FileSystemError::InvalidPath,
    FileSystemError::BadDescriptor,
    FileSystemError::TooManyOpenFiles,
    FileSystemError::ReadOnly,
    FileSystemError::NoSpace,
    FileSystemError::CrossMount,
    FileSystemError::NoSession,
    FileSystemError::NoBuffer,
    FileSystemError::BufferTooSmall,
    FileSystemError::NotSupported,
    FileSystemError::Unresponsive,
    FileSystemError::WrongMode,
];

impl FileSystemError {
    const fn discriminant(self) -> u16 {
        match self {
            Self::NotFound => 1,
            Self::NotADirectory => 2,
            Self::IsADirectory => 3,
            Self::NotEmpty => 4,
            Self::AlreadyExists => 5,
            Self::InvalidPath => 6,
            Self::BadDescriptor => 7,
            Self::TooManyOpenFiles => 8,
            Self::ReadOnly => 9,
            Self::NoSpace => 10,
            Self::CrossMount => 11,
            Self::NoSession => 12,
            Self::NoBuffer => 13,
            Self::BufferTooSmall => 14,
            Self::NotSupported => 15,
            Self::Unresponsive => 16,
            Self::WrongMode => 17,
        }
    }

    fn from_discriminant(value: u16) -> Option<Self> {
        VARIANTS
            .into_iter()
            .find(|variant| variant.discriminant() == value)
    }
}

impl<'a> WireValue<'a> for FileSystemError {
    fn write_as_field<const N: usize>(
        &self,
        buf: &mut MessageBuf<N>,
        field_id: u8,
        _handles: &mut HandleSlots,
    ) -> Result<(), IpcError> {
        buf.write_field(field_id, &value::encode_discriminant(self.discriminant()))
    }

    fn from_field(data: &'a [u8], _handles: &HandleSlots) -> Result<Self, IpcError> {
        Self::from_discriminant(value::decode_discriminant(data)?).ok_or(IpcError::InvalidValue)
    }
}

impl WireTyped for FileSystemError {
    const WIRE_TYPE: WireType = WireType::Uint(ENUM_DISCRIMINANT_SIZE as u8);
}

#[cfg(test)]
mod tests {
    use ipc::{
        WireValue,
        wire::{FieldCursor, HandleSlots, IpcError, MESSAGE_INLINE_MAX, MessageBuf, value},
    };

    use super::{FileSystemError, VARIANTS};

    const FIELD_ID: u8 = 1;

    fn round_trip(error: FileSystemError) -> Result<FileSystemError, IpcError> {
        let mut buf = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        error
            .write_as_field(&mut buf, FIELD_ID, &mut HandleSlots::new())
            .expect("write ok");
        buf.finish().expect("finish ok");
        let mut cursor = FieldCursor::new(buf.as_bytes());
        let field = cursor.next_field().expect("decode ok").expect("present");
        assert_eq!(field.id, FIELD_ID);
        FileSystemError::from_field(field.data, &HandleSlots::new())
    }

    fn decode(discriminant: u16) -> Result<FileSystemError, IpcError> {
        FileSystemError::from_field(
            &value::encode_discriminant(discriminant),
            &HandleSlots::new(),
        )
    }

    #[test]
    fn every_variant_round_trips() {
        for variant in VARIANTS {
            assert_eq!(round_trip(variant), Ok(variant));
        }
    }

    #[test]
    fn discriminants_are_distinct_and_dense() {
        for (index, variant) in VARIANTS.into_iter().enumerate() {
            assert_eq!(variant.discriminant() as usize, index + 1);
        }
    }

    #[test]
    fn discriminant_outside_the_set_is_rejected() {
        assert_eq!(decode(0), Err(IpcError::InvalidValue));
        assert_eq!(
            decode(VARIANTS.len() as u16 + 1),
            Err(IpcError::InvalidValue)
        );
        assert_eq!(decode(u16::MAX), Err(IpcError::InvalidValue));
    }

    #[test]
    fn wrong_field_length_is_rejected() {
        assert_eq!(
            FileSystemError::from_field(&[1], &HandleSlots::new()),
            Err(IpcError::InvalidValue)
        );
        assert_eq!(
            FileSystemError::from_field(&[1, 0, 0], &HandleSlots::new()),
            Err(IpcError::InvalidValue)
        );
    }
}
