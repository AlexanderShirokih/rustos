use kernel_objects::{MemoryRegion, ReadOnlyMapping, Result};
use runtime::MemoryAccess;
use userland::{SegmentPermissions, SegmentView, USERLAND_PAGE_SIZE};

/// Отображение userland-образа только для чтения, заимствованное у владельца. Смещение байтов
/// сегмента в отображении загрузчик берёт за смещение в регионе образа.
#[derive(Debug, Clone, Copy)]
pub struct ImagePages<'a> {
    mapping: &'a ReadOnlyMapping,
}

impl<'a> ImagePages<'a> {
    /// Страницы образа из его отображения `mapping`.
    pub const fn new(mapping: &'a ReadOnlyMapping) -> Self {
        Self { mapping }
    }

    pub(crate) fn segment_slice<'s>(
        self,
        segment: impl SegmentView<'s>,
        mapped: u64,
    ) -> Result<Option<MemoryRegion>> {
        let access = match segment.permissions() {
            SegmentPermissions::ReadWrite => return Ok(None),
            SegmentPermissions::ReadOnly => MemoryAccess::READ,
            SegmentPermissions::ReadExecute => MemoryAccess::RX,
        };

        let bytes = segment.bytes();
        let Some(offset) = self.offset_of(bytes) else {
            return Ok(None);
        };

        let covers_memory = u64::try_from(bytes.len()).is_ok_and(|len| len >= segment.mem_size());
        if offset % USERLAND_PAGE_SIZE != 0 || !covers_memory {
            return Ok(None);
        }

        self.mapping
            .region()
            .slice(offset, mapped, access)
            .map(Some)
    }

    fn offset_of(self, bytes: &[u8]) -> Option<u64> {
        let image = self.mapping.as_bytes();
        let offset = bytes.as_ptr().addr().checked_sub(image.as_ptr().addr())?;
        let end = offset.checked_add(bytes.len())?;
        if end > image.len() {
            return None;
        }
        u64::try_from(offset).ok()
    }
}
