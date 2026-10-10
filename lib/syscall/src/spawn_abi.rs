//! ABI описания для `ProcessLoadImage`/`ProcessStart`.
//!
//! Все поля little-endian, без выравнивания. Декодеры читают по полям
//! через `u32::from_le_bytes`/`u64::from_le_bytes`, не полагаясь на
//! `repr(C)`-layout.

/// Единственная версия, которую принимает `ProcessLoadImage` в поле
/// [`UserImageDescAbi::version`]. Другое значение даёт `InvalidArgument`.
pub const SEGMENT_ABI_VERSION: u32 = 1;

/// Потолок [`UserImageDescAbi::segment_count`]. Пустой и превышающий потолок образ
/// `ProcessLoadImage` отвергает как `InvalidArgument`.
pub const MAX_SEGMENTS_PER_IMG: usize = 16;

/// Ёмкость имени процесса в `ProcessCreate`, в байтах UTF-8. Длина вне `1..=64` даёт
/// `InvalidArgument`.
pub const MAX_PROCESS_NAME_LEN: usize = 64;

/// Размер сериализованной [`UserImageDescAbi`] в байтах: `4 + 4 + 8*4 = 40`. Аргумент
/// `desc_len` у `ProcessLoadImage` равен этому размеру, иначе - `InvalidArgument`.
pub const USER_IMAGE_DESC_SIZE: usize = 40;
/// Размер сериализованной [`UserSegmentAbi`] в байтах: `4 + 4 + 8*3 = 32`. Шаг массива
/// сегментов по [`UserImageDescAbi::segments_va`] равен этому размеру.
pub const USER_SEGMENT_SIZE: usize = 32;

/// Описание образа: версия ABI, ссылка на массив сегментов, параметры
/// стека и user-vm.
///
/// Смещения полей в сериализованном виде: `version` в `[0..4)`, `segment_count` в
/// `[4..8)`, `segments_va` в `[8..16)`, `entry_va` в `[16..24)`, `user_vm_base` в
/// `[24..32)`, `user_vm_size` в `[32..40)`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserImageDescAbi {
    /// Версия ABI. Принимается только [`SEGMENT_ABI_VERSION`].
    pub version: u32,
    /// Число сегментов. Должно лежать в `1..=MAX_SEGMENTS_PER_IMG`.
    pub segment_count: u32,
    /// User-указатель на массив из `segment_count` сериализованных [`UserSegmentAbi`]
    /// по [`USER_SEGMENT_SIZE`] байт каждая. Ядро копирует массив до возврата из syscall.
    pub segments_va: u64,
    /// PC первой user-инструкции.
    pub entry_va: u64,
    /// База диапазона user_vm-аллокатора (4K-выровнена).
    pub user_vm_base: u64,
    /// Размер диапазона user_vm-аллокатора.
    pub user_vm_size: u64,
}

/// Один сегмент: handle на `Memory`-регион в loader-таблице + флаги
/// доступа + VA в child AS + размер.
///
/// Смещения полей в сериализованном виде: `region_handle` в `[0..4)`, `flags` в
/// `[4..8)`, `va_base` в `[8..16)`, `mapped_size` в `[16..24)`, `reserved` в `[24..32)`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserSegmentAbi {
    /// `HandleId` региона в таблице вызывающего `ProcessLoadImage`. Handle остаётся у
    /// вызывающего, ядро устанавливает регион в child AS по нему. Требует `Rights::TRANSFER`
    /// и права по `flags`: `READ | WRITE` для RW, `READ` для RO, `READ | EXECUTE` для RX.
    pub region_handle: u32,
    /// Флаги: `0 = RW`, `1 = RO`, `2 = RX` (см. [`UserMemFlags`](crate::UserMemFlags)).
    /// Значение вне этих трёх даёт `InvalidArgument`.
    pub flags: u32,
    /// База в child AS (4K-выровнена).
    pub va_base: u64,
    /// Замапливаемый размер. Расхождение с размером региона даёт `InvalidArgument`.
    pub mapped_size: u64,
    /// Зарезервировано. Ненулевое значение даёт `InvalidArgument`.
    pub reserved: u64,
}

/// Декодирует первые [`USER_IMAGE_DESC_SIZE`] байт в [`UserImageDescAbi`], хвост за ними
/// не разбирает. `None` на буфере короче этого размера.
pub fn decode_image_desc(bytes: &[u8]) -> Option<UserImageDescAbi> {
    let (version, rest) = bytes.split_first_chunk::<4>()?;
    let (segment_count, rest) = rest.split_first_chunk::<4>()?;
    let (segments_va, rest) = rest.split_first_chunk::<8>()?;
    let (entry_va, rest) = rest.split_first_chunk::<8>()?;
    let (user_vm_base, rest) = rest.split_first_chunk::<8>()?;
    let (user_vm_size, _) = rest.split_first_chunk::<8>()?;
    Some(UserImageDescAbi {
        version: u32::from_le_bytes(*version),
        segment_count: u32::from_le_bytes(*segment_count),
        segments_va: u64::from_le_bytes(*segments_va),
        entry_va: u64::from_le_bytes(*entry_va),
        user_vm_base: u64::from_le_bytes(*user_vm_base),
        user_vm_size: u64::from_le_bytes(*user_vm_size),
    })
}

/// Декодирует первые [`USER_SEGMENT_SIZE`] байт в [`UserSegmentAbi`], хвост за ними не
/// разбирает. `None` на буфере короче этого размера.
pub fn decode_segment(bytes: &[u8]) -> Option<UserSegmentAbi> {
    let (region_handle, rest) = bytes.split_first_chunk::<4>()?;
    let (flags, rest) = rest.split_first_chunk::<4>()?;
    let (va_base, rest) = rest.split_first_chunk::<8>()?;
    let (mapped_size, rest) = rest.split_first_chunk::<8>()?;
    let (reserved, _) = rest.split_first_chunk::<8>()?;
    Some(UserSegmentAbi {
        region_handle: u32::from_le_bytes(*region_handle),
        flags: u32::from_le_bytes(*flags),
        va_base: u64::from_le_bytes(*va_base),
        mapped_size: u64::from_le_bytes(*mapped_size),
        reserved: u64::from_le_bytes(*reserved),
    })
}

/// Кодирует [`UserImageDescAbi`] в ровно [`USER_IMAGE_DESC_SIZE`] байт.
pub fn encode_image_desc(desc: &UserImageDescAbi) -> [u8; USER_IMAGE_DESC_SIZE] {
    let mut bytes = [0u8; USER_IMAGE_DESC_SIZE];
    bytes[0..4].copy_from_slice(&desc.version.to_le_bytes());
    bytes[4..8].copy_from_slice(&desc.segment_count.to_le_bytes());
    bytes[8..16].copy_from_slice(&desc.segments_va.to_le_bytes());
    bytes[16..24].copy_from_slice(&desc.entry_va.to_le_bytes());
    bytes[24..32].copy_from_slice(&desc.user_vm_base.to_le_bytes());
    bytes[32..40].copy_from_slice(&desc.user_vm_size.to_le_bytes());
    bytes
}

/// Кодирует [`UserSegmentAbi`] в ровно [`USER_SEGMENT_SIZE`] байт.
pub fn encode_segment(seg: &UserSegmentAbi) -> [u8; USER_SEGMENT_SIZE] {
    let mut bytes = [0u8; USER_SEGMENT_SIZE];
    bytes[0..4].copy_from_slice(&seg.region_handle.to_le_bytes());
    bytes[4..8].copy_from_slice(&seg.flags.to_le_bytes());
    bytes[8..16].copy_from_slice(&seg.va_base.to_le_bytes());
    bytes[16..24].copy_from_slice(&seg.mapped_size.to_le_bytes());
    bytes[24..32].copy_from_slice(&seg.reserved.to_le_bytes());
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_image_desc_size_matches_field_layout() {
        assert_eq!(USER_IMAGE_DESC_SIZE, 4 + 4 + 8 * 4);
    }

    #[test]
    fn user_segment_size_matches_field_layout() {
        assert_eq!(USER_SEGMENT_SIZE, 4 + 4 + 8 * 3);
    }

    #[test]
    fn decode_image_desc_round_trip() {
        let mut full = [0u8; USER_IMAGE_DESC_SIZE];
        full[0..4].copy_from_slice(&1u32.to_le_bytes());
        full[4..8].copy_from_slice(&3u32.to_le_bytes());
        full[8..16].copy_from_slice(&0x1234u64.to_le_bytes());
        full[16..24].copy_from_slice(&0x4000_0000u64.to_le_bytes());
        full[24..32].copy_from_slice(&0x6000_0000u64.to_le_bytes());
        full[32..40].copy_from_slice(&0x1_0000u64.to_le_bytes());
        let desc = decode_image_desc(&full).expect("decode ok");
        assert_eq!(desc.version, 1);
        assert_eq!(desc.segment_count, 3);
        assert_eq!(desc.segments_va, 0x1234);
        assert_eq!(desc.entry_va, 0x4000_0000);
        assert_eq!(desc.user_vm_base, 0x6000_0000);
        assert_eq!(desc.user_vm_size, 0x1_0000);
    }

    #[test]
    fn decode_image_desc_rejects_short_buffer() {
        let bytes = [0u8; 20];
        assert!(decode_image_desc(&bytes).is_none());
    }

    #[test]
    fn decode_segment_round_trip() {
        let mut bytes = [0u8; USER_SEGMENT_SIZE];
        bytes[0..4].copy_from_slice(&0x42u32.to_le_bytes());
        bytes[4..8].copy_from_slice(&2u32.to_le_bytes());
        bytes[8..16].copy_from_slice(&0x4000_0000u64.to_le_bytes());
        bytes[16..24].copy_from_slice(&0x1000u64.to_le_bytes());
        let seg = decode_segment(&bytes).expect("decode ok");
        assert_eq!(seg.region_handle, 0x42);
        assert_eq!(seg.flags, 2);
        assert_eq!(seg.va_base, 0x4000_0000);
        assert_eq!(seg.mapped_size, 0x1000);
        assert_eq!(seg.reserved, 0);
    }

    #[test]
    fn decode_segment_rejects_short_buffer() {
        let bytes = [0u8; 8];
        assert!(decode_segment(&bytes).is_none());
    }

    #[test]
    fn encode_image_desc_round_trip() {
        let desc = UserImageDescAbi {
            version: 1,
            segment_count: 7,
            segments_va: 0x1111_2222_3333_4444,
            entry_va: 0x5555_6666_7777_8888,
            user_vm_base: 0xdddd_eeee_ffff_0000,
            user_vm_size: 0x0a0b_0c0d_0e0f_1011,
        };
        let bytes = encode_image_desc(&desc);
        assert_eq!(decode_image_desc(&bytes), Some(desc));
    }

    #[test]
    fn encode_segment_round_trip() {
        let seg = UserSegmentAbi {
            region_handle: 0xdead_beef,
            flags: 1,
            va_base: 0x1234_5678_9abc_def0,
            mapped_size: 0x0fed_cba9_8765_4321,
            reserved: 0xa5a5_5a5a_c3c3_3c3c,
        };
        let bytes = encode_segment(&seg);
        assert_eq!(decode_segment(&bytes), Some(seg));
    }
}
