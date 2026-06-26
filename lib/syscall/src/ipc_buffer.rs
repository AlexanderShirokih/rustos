//! Per-thread IPC-буфер: страница user-памяти, через которую
//! Port-syscalls обмениваются короткими сообщениями.
//!
//! Структура [`IpcBuffer`] разделяется ядром и userspace. Буфер маппится ядром по
//! одной странице на каждый user-поток; userspace получает его VA через
//! `ThreadIpcBufferAddr`-syscall.
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

use core::cmp::min;

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

/// Per-thread IPC-буфер.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct IpcBuffer {
    /// `len(bytes)` в `[0..16)`, `ncaps` в `[16..20)`, флаги в `[20..)`.
    pub tag: u64,
    /// HandleId'ы для переноса/приёма.
    pub caps: [u32; IPC_BUFFER_MAX_CAPS],
    /// Тело сообщения.
    pub data: [u8; IPC_BUFFER_DATA_MAX],
    /// Badge отправителя, через который пришло сообщение. При отправке игнорируется.
    pub badge: u64,
}

impl IpcBuffer {
    /// Создает устой буфер.
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
pub const fn encode_tag(len: usize, ncaps: usize) -> u64 {
    let len = min(len, IPC_BUFFER_DATA_MAX);
    let ncaps = min(ncaps, IPC_BUFFER_MAX_CAPS);

    (len as u64 & TAG_LEN_MASK) | ((ncaps as u64 & TAG_NCAPS_MASK) << TAG_NCAPS_SHIFT)
}

/// Распакованные поля [`IpcBuffer::tag`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DecodedTag {
    pub len: usize,
    pub ncaps: usize,
}

/// Распаковывает `tag` обратно в [`DecodedTag`].
pub const fn decode_tag(tag: u64) -> DecodedTag {
    DecodedTag {
        len: (tag & TAG_LEN_MASK) as usize,
        ncaps: ((tag >> TAG_NCAPS_SHIFT) & TAG_NCAPS_MASK) as usize,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_fits_one_page() {
        assert!(size_of::<IpcBuffer>() <= 4096);
    }

    #[test]
    fn tag_roundtrip() {
        assert_eq!(
            decode_tag(encode_tag(0, 0)),
            DecodedTag { len: 0, ncaps: 0 }
        );
        assert_eq!(
            decode_tag(encode_tag(200, 3)),
            DecodedTag { len: 200, ncaps: 3 }
        );
        assert_eq!(
            decode_tag(encode_tag(IPC_BUFFER_DATA_MAX, IPC_BUFFER_MAX_CAPS)),
            DecodedTag {
                len: IPC_BUFFER_DATA_MAX,
                ncaps: IPC_BUFFER_MAX_CAPS
            }
        );
    }

    #[test]
    fn tag_clamps_oversized_fields() {
        let decoded = decode_tag(encode_tag(10_000, 99));
        assert_eq!(decoded.len, IPC_BUFFER_DATA_MAX);
        assert_eq!(decoded.ncaps, IPC_BUFFER_MAX_CAPS);
    }

    #[test]
    fn layout_offsets_stable() {
        assert_eq!(core::mem::offset_of!(IpcBuffer, tag), 0);
        assert_eq!(core::mem::offset_of!(IpcBuffer, caps), 8);
        assert_eq!(
            core::mem::offset_of!(IpcBuffer, data),
            8 + IPC_BUFFER_MAX_CAPS * 4
        );
        assert_eq!(
            core::mem::offset_of!(IpcBuffer, badge),
            8 + IPC_BUFFER_MAX_CAPS * 4 + IPC_BUFFER_DATA_MAX
        );
    }
}
