//! Per-thread IPC-буфер: страница user-памяти, через которую
//! Port-syscalls обмениваются короткими сообщениями без полного
//! channel-механизма.
//!
//! Структура [`IpcBuffer`] разделяется ядром и userspace - её layout
//! фиксирован `#[repr(C)]` и является частью ABI. Буфер маппится ядром по
//! одной странице на каждый user-поток; userspace получает его VA через
//! `IpcBufferAddr`-syscall.
//!
//! # Tag
//!
//! Поле `tag` упаковывает метаданные сообщения:
//!
//! | биты      | значение                          |
//! |-----------|-----------------------------------|
//! | `[0..16)` | `len` - длина тела в `data` (байт)|
//! | `[16..20)`| `ncaps` - число handle'ов в `caps`|
//! | `[20..)`  | флаги (резерв)                    |
//!
//! `len` ограничен [`IPC_BUFFER_DATA_MAX`], `ncaps` - [`IPC_BUFFER_MAX_CAPS`];
//! оба укладываются в выделенные битовые поля.

/// Максимум байт тела сообщения в [`IpcBuffer::data`].
pub const IPC_BUFFER_DATA_MAX: usize = 256;

/// Максимум переносимых/принимаемых handle'ов в [`IpcBuffer::caps`].
pub const IPC_BUFFER_MAX_CAPS: usize = 4;

/// Сдвиг поля `ncaps` в [`IpcBuffer::tag`].
const TAG_NCAPS_SHIFT: u32 = 16;
/// Маска поля `len` (`[0..16)`) в `tag`.
const TAG_LEN_MASK: u64 = 0xFFFF;
/// Маска поля `ncaps` после сдвига (`[16..20)`, 4 бита).
const TAG_NCAPS_MASK: u64 = 0xF;

/// Per-thread IPC-буфер. Layout фиксирован ABI.
///
/// `tag` несёт длину тела и число handle'ов (см. [`encode_tag`]/[`decode_tag`]),
/// `caps` - сырые HandleId'ы, `data` - тело сообщения, `badge` - значок
/// отправителя, доставляемый ядром получателю на recv/call. Размер
/// укладывается в одну страницу (4096 байт).
///
/// `badge` положен после `data`, чтобы офсеты `tag`/`caps`/`data` оставались
/// стабильными при расширении буфера.
/// `badge` - выходное поле получателя: ядро пишет туда значок хендла
/// отправителя после успешного рандеву. На отправке не читается.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct IpcBuffer {
    /// `len(bytes)` в `[0..16)`, `ncaps` в `[16..20)`, флаги в `[20..)`.
    pub tag: u64,
    /// HandleId'ы для переноса/приёма (raw 32-бит значения).
    pub caps: [u32; IPC_BUFFER_MAX_CAPS],
    /// Тело сообщения.
    pub data: [u8; IPC_BUFFER_DATA_MAX],
    /// Значок (badge) отправителя: ядро записывает сюда badge port-хендла,
    /// через который пришло сообщение (`0` = без значка). Идентифицирует
    /// клиента/соединение на стороне сервера. На отправке игнорируется.
    pub badge: u64,
}

impl IpcBuffer {
    /// Пустой буфер: нулевой tag, обнулённые `caps`/`data`/`badge`.
    pub const fn zeroed() -> Self {
        Self {
            tag: 0,
            caps: [0; IPC_BUFFER_MAX_CAPS],
            data: [0; IPC_BUFFER_DATA_MAX],
            badge: 0,
        }
    }
}

/// Упаковывает `len` (длина тела) и `ncaps` (число handle'ов) в `tag`.
/// `len` усекается по [`IPC_BUFFER_DATA_MAX`], `ncaps` - по
/// [`IPC_BUFFER_MAX_CAPS`].
pub const fn encode_tag(len: usize, ncaps: usize) -> u64 {
    let len = if len > IPC_BUFFER_DATA_MAX {
        IPC_BUFFER_DATA_MAX
    } else {
        len
    };
    let ncaps = if ncaps > IPC_BUFFER_MAX_CAPS {
        IPC_BUFFER_MAX_CAPS
    } else {
        ncaps
    };
    (len as u64 & TAG_LEN_MASK) | ((ncaps as u64 & TAG_NCAPS_MASK) << TAG_NCAPS_SHIFT)
}

/// Распаковывает `tag` обратно в `(len, ncaps)`.
pub const fn decode_tag(tag: u64) -> (usize, usize) {
    let len = (tag & TAG_LEN_MASK) as usize;
    let ncaps = ((tag >> TAG_NCAPS_SHIFT) & TAG_NCAPS_MASK) as usize;
    (len, ncaps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_fits_one_page() {
        assert!(core::mem::size_of::<IpcBuffer>() <= 4096);
    }

    #[test]
    fn tag_roundtrip() {
        assert_eq!(decode_tag(encode_tag(0, 0)), (0, 0));
        assert_eq!(decode_tag(encode_tag(200, 3)), (200, 3));
        assert_eq!(
            decode_tag(encode_tag(IPC_BUFFER_DATA_MAX, IPC_BUFFER_MAX_CAPS)),
            (IPC_BUFFER_DATA_MAX, IPC_BUFFER_MAX_CAPS)
        );
    }

    #[test]
    fn tag_clamps_oversized_fields() {
        let (len, ncaps) = decode_tag(encode_tag(10_000, 99));
        assert_eq!(len, IPC_BUFFER_DATA_MAX);
        assert_eq!(ncaps, IPC_BUFFER_MAX_CAPS);
    }

    #[test]
    fn layout_offsets_stable() {
        assert_eq!(core::mem::offset_of!(IpcBuffer, tag), 0);
        assert_eq!(core::mem::offset_of!(IpcBuffer, caps), 8);
        assert_eq!(
            core::mem::offset_of!(IpcBuffer, data),
            8 + IPC_BUFFER_MAX_CAPS * 4
        );
        // badge лежит сразу за data: 8 (tag) + 16 (caps) + 256 (data) = 280.
        assert_eq!(
            core::mem::offset_of!(IpcBuffer, badge),
            8 + IPC_BUFFER_MAX_CAPS * 4 + IPC_BUFFER_DATA_MAX
        );
    }
}
