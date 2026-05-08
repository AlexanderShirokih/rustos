//! Загрузка `UserImage` (определён в `drivers_common::services::user_image`)
//! в адресное пространство user-AS через `MemoryMapper`.

use drivers_common::services::user_image::{UserImage, UserImageError};
use memory::{MemFlags, memory_mapper::MemoryMapper};

const FRAME_SIZE: usize = 4096;

/// Маппит все сегменты и user-стек `image` в адресное пространство `mapper`.
/// Mapper сам аллоцирует физ. фреймы и копирует `init_bytes` сегмента в
/// начало региона; user-стек получает `MemFlags::user_rw` без init.
///
/// На ошибке любого шага все ранее замапленные страницы остаются в `mapper`-е
/// (он сам освободит их при дропе AS) - вызывающему достаточно сбросить
/// `Arc<AddressSpace>`.
pub(crate) fn load_user_image(
    mapper: &(dyn MemoryMapper + Send + Sync),
    image: &UserImage<'_>,
) -> Result<(), UserImageError> {
    image.validate()?;

    for seg in image.segments {
        mapper.map(
            seg.va_base,
            seg.mapped_size / FRAME_SIZE,
            seg.init_bytes,
            seg.perms,
        )?;
    }

    let stack_base = image.user_stack_base()?;
    mapper.map(
        stack_base,
        image.user_stack_size / FRAME_SIZE,
        &[],
        MemFlags::user_rw(),
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::{sync::Mutex, vec};

    use drivers_common::services::user_image::UserSegment;
    use memory::{
        memory_mapper::{
            AddressSpaceTag, MemoryMapper, MemoryMappingError, MemoryRemappingError,
            MemoryUnmappingError,
        },
        physical_address::{PageAlignedAddress, PhysicalAddress},
        virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
    };

    use super::*;

    const PAGE: usize = FRAME_SIZE;

    fn aligned(addr: usize) -> PageAlignedVirtualAddress {
        PageAlignedVirtualAddress::from_usize(addr).expect("aligned addr in test")
    }

    fn rx_segment<'a>(va: usize, size: usize, init: &'a [u8]) -> UserSegment<'a> {
        UserSegment {
            va_base: aligned(va),
            mapped_size: size,
            init_bytes: init,
            perms: MemFlags::user_rx(),
        }
    }

    fn rw_segment<'a>(va: usize, size: usize, init: &'a [u8]) -> UserSegment<'a> {
        UserSegment {
            va_base: aligned(va),
            mapped_size: size,
            init_bytes: init,
            perms: MemFlags::user_rw(),
        }
    }

    fn make_image<'a>(segs: &'a [UserSegment<'a>], entry: usize) -> UserImage<'a> {
        UserImage {
            segments: segs,
            entry: VirtualAddress::new(entry),
            user_stack_top: VirtualAddress::new(0x1_0000_0000),
            user_stack_size: 4 * PAGE,
        }
    }

    // Mock mapper для load_user_image

    #[derive(Debug, PartialEq, Eq, Clone)]
    struct MapCall {
        va: usize,
        page_count: usize,
        init_hash: u64,
        init_len: usize,
    }

    struct MockMapper {
        calls: Mutex<std::vec::Vec<MapCall>>,
    }

    impl MockMapper {
        fn new() -> Self {
            Self {
                calls: Mutex::new(std::vec::Vec::new()),
            }
        }
        fn calls(&self) -> std::vec::Vec<MapCall> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl MemoryMapper for MockMapper {
        fn map(
            &self,
            va: PageAlignedVirtualAddress,
            page_count: usize,
            init: &[u8],
            _: MemFlags,
        ) -> Result<(), MemoryMappingError> {
            self.calls.lock().unwrap().push(MapCall {
                va: va.as_usize(),
                page_count,
                init_hash: fnv1a(init),
                init_len: init.len(),
            });
            Ok(())
        }
        fn map_exact(
            &self,
            _: PageAlignedVirtualAddress,
            _: PageAlignedAddress,
            _: usize,
            _: MemFlags,
        ) -> Result<(), MemoryMappingError> {
            unreachable!()
        }
        fn unmap(
            &self,
            _: PageAlignedVirtualAddress,
            _: usize,
        ) -> Result<(), MemoryUnmappingError> {
            unreachable!()
        }
        fn remap(
            &self,
            _: PageAlignedVirtualAddress,
            _: usize,
            _: MemFlags,
        ) -> Result<(), MemoryRemappingError> {
            unreachable!()
        }
        fn activate_handle(&self) -> memory::memory_mapper::AddressSpaceHandle {
            memory::memory_mapper::AddressSpaceHandle::new(
                PhysicalAddress::new(0xDEADBEEF),
                AddressSpaceTag::NONE,
            )
        }
    }

    fn fnv1a(bytes: &[u8]) -> u64 {
        let mut hash: u64 = 0xcbf29ce484222325;
        for &b in bytes {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }

    #[test]
    fn load_user_image_maps_segments_and_stack_in_order() {
        let init = vec![0xCDu8; 100];
        let segs = [rx_segment(0x4000_0000, PAGE, &init)];
        let image = make_image(&segs, 0x4000_0000);

        let mapper = MockMapper::new();

        load_user_image(&mapper, &image).expect("load ok");

        let calls = mapper.calls();
        // 1 сегмент -> 1 map(); user-стек -> 1 map() с page_count=4.
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].va, 0x4000_0000);
        assert_eq!(calls[0].page_count, 1);
        assert_eq!(calls[0].init_len, 100);

        let stack_base = 0x1_0000_0000 - 4 * PAGE;
        assert_eq!(calls[1].va, stack_base);
        assert_eq!(calls[1].page_count, 4);
        assert_eq!(calls[1].init_len, 0);
    }

    #[test]
    fn load_user_image_passes_init_bytes_to_mapper() {
        let init = vec![0xABu8; 8];
        let segs = [rx_segment(0x4000_0000, PAGE, &init)];
        let image = make_image(&segs, 0x4000_0000);

        let mapper = MockMapper::new();
        load_user_image(&mapper, &image).unwrap();

        let calls = mapper.calls();
        assert_eq!(calls[0].init_hash, fnv1a(&init));
    }

    #[test]
    fn load_user_image_rejects_invalid_image() {
        let segs = [rw_segment(0x4000_0000, PAGE, &[])];
        let image = make_image(&segs, 0x4000_0000);
        let mapper = MockMapper::new();
        assert_eq!(
            load_user_image(&mapper, &image),
            Err(UserImageError::EntryNotInExecSegment)
        );
        assert!(mapper.calls().is_empty());
    }
}
