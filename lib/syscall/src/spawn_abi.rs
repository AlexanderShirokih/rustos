//! ABI описания для `ProcessLoadImage`/`ProcessStart`.
//!
//! Все поля little-endian, без выравнивания; декодеры читают по полям
//! через `u32::from_le_bytes`/`u64::from_le_bytes`, не полагаясь на
//! `repr(C)`-layout.

pub const SEGMENT_ABI_VERSION: u32 = 1;
pub const MAX_SEGMENTS_PER_IMG: usize = 16;
pub const MAX_BOOTSTRAP_HANDLES: usize = 32;

/// Размер сериализованной `UserImageDescAbi` в байтах: `4 + 4 + 8*6 = 56`.
pub const USER_IMAGE_DESC_SIZE: usize = 56;
/// Размер сериализованной `UserSegmentAbi` в байтах.
pub const USER_SEGMENT_SIZE: usize = 32;

/// Описание образа: версия ABI, ссылка на массив сегментов, параметры
/// стека и user-vm.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserImageDescAbi {
    /// Версия ABI; принимается только [`SEGMENT_ABI_VERSION`].
    pub version: u32,
    /// Число сегментов; должно лежать в `1..=MAX_SEGMENTS_PER_IMG`.
    pub segment_count: u32,
    /// User-указатель на массив сегментов длиной `segment_count`.
    pub segments_va: u64,
    /// PC первой user-инструкции.
    pub entry_va: u64,
    /// Вершина user-стека (4K-выровнена).
    pub user_stack_top: u64,
    /// Размер user-стека (4K-кратен, ненулевой).
    pub user_stack_size: u64,
    /// База диапазона user_vm-аллокатора (4K-выровнена).
    pub user_vm_base: u64,
    /// Размер диапазона user_vm-аллокатора.
    pub user_vm_size: u64,
}

/// Один сегмент: handle на `Memory`-регион в loader-таблице + флаги
/// доступа + VA в child AS + размер.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserSegmentAbi {
    /// `HandleId` региона в loader-таблице.
    pub region_handle: u32,
    /// Флаги: `0 = RW`, `1 = RO`, `2 = RX` (см. [`UserMemFlags`](crate::UserMemFlags)).
    pub flags: u32,
    /// База в child AS (4K-выровнена).
    pub va_base: u64,
    /// Замапливаемый размер (равен `region.size_bytes()`).
    pub mapped_size: u64,
    /// Зарезервировано; должно быть `0`.
    pub reserved: u64,
}

/// Декодирует ровно [`USER_IMAGE_DESC_SIZE`] байт в `UserImageDescAbi`.
/// `None`, если буфер недостаточен.
pub fn decode_image_desc(bytes: &[u8]) -> Option<UserImageDescAbi> {
    if bytes.len() < USER_IMAGE_DESC_SIZE {
        return None;
    }
    Some(UserImageDescAbi {
        version: u32::from_le_bytes(bytes[0..4].try_into().ok()?),
        segment_count: u32::from_le_bytes(bytes[4..8].try_into().ok()?),
        segments_va: u64::from_le_bytes(bytes[8..16].try_into().ok()?),
        entry_va: u64::from_le_bytes(bytes[16..24].try_into().ok()?),
        user_stack_top: u64::from_le_bytes(bytes[24..32].try_into().ok()?),
        user_stack_size: u64::from_le_bytes(bytes[32..40].try_into().ok()?),
        user_vm_base: u64::from_le_bytes(bytes[40..48].try_into().ok()?),
        user_vm_size: u64::from_le_bytes(bytes[48..56].try_into().ok()?),
    })
}

/// Декодирует ровно [`USER_SEGMENT_SIZE`] байт в `UserSegmentAbi`.
/// `None`, если буфер недостаточен.
pub fn decode_segment(bytes: &[u8]) -> Option<UserSegmentAbi> {
    if bytes.len() < USER_SEGMENT_SIZE {
        return None;
    }
    Some(UserSegmentAbi {
        region_handle: u32::from_le_bytes(bytes[0..4].try_into().ok()?),
        flags: u32::from_le_bytes(bytes[4..8].try_into().ok()?),
        va_base: u64::from_le_bytes(bytes[8..16].try_into().ok()?),
        mapped_size: u64::from_le_bytes(bytes[16..24].try_into().ok()?),
        reserved: u64::from_le_bytes(bytes[24..32].try_into().ok()?),
    })
}

/// Кодирует `UserImageDescAbi` в ровно [`USER_IMAGE_DESC_SIZE`] байт.
pub fn encode_image_desc(desc: &UserImageDescAbi) -> [u8; USER_IMAGE_DESC_SIZE] {
    let mut bytes = [0u8; USER_IMAGE_DESC_SIZE];
    bytes[0..4].copy_from_slice(&desc.version.to_le_bytes());
    bytes[4..8].copy_from_slice(&desc.segment_count.to_le_bytes());
    bytes[8..16].copy_from_slice(&desc.segments_va.to_le_bytes());
    bytes[16..24].copy_from_slice(&desc.entry_va.to_le_bytes());
    bytes[24..32].copy_from_slice(&desc.user_stack_top.to_le_bytes());
    bytes[32..40].copy_from_slice(&desc.user_stack_size.to_le_bytes());
    bytes[40..48].copy_from_slice(&desc.user_vm_base.to_le_bytes());
    bytes[48..56].copy_from_slice(&desc.user_vm_size.to_le_bytes());
    bytes
}

/// Кодирует `UserSegmentAbi` в ровно [`USER_SEGMENT_SIZE`] байт.
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
        assert_eq!(USER_IMAGE_DESC_SIZE, 4 + 4 + 8 * 6);
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
        full[24..32].copy_from_slice(&0x5000_0000u64.to_le_bytes());
        full[32..40].copy_from_slice(&0x4000u64.to_le_bytes());
        full[40..48].copy_from_slice(&0x6000_0000u64.to_le_bytes());
        full[48..56].copy_from_slice(&0x1_0000u64.to_le_bytes());
        let desc = decode_image_desc(&full).expect("decode ok");
        assert_eq!(desc.version, 1);
        assert_eq!(desc.segment_count, 3);
        assert_eq!(desc.segments_va, 0x1234);
        assert_eq!(desc.entry_va, 0x4000_0000);
        assert_eq!(desc.user_stack_top, 0x5000_0000);
        assert_eq!(desc.user_stack_size, 0x4000);
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
            user_stack_top: 0x9999_aaaa_bbbb_cccc,
            user_stack_size: 0x0001_0002_0003_0004,
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
