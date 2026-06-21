//! Загрузка [`UserImage`] в адресное пространство user-AS через
//! [`MemoryMapper`].

use memory::{MemFlags, memory_mapper::MemoryMapper};

use crate::image::{UserImage, UserImageError};

const FRAME_SIZE: usize = 4096;

/// Маппит все сегменты и user-стек `image` в `mapper` (аллокацию фреймов и
/// копирование `init_bytes` делает сам mapper). На ошибке любого шага уже
/// замапленные страницы остаются в `mapper`-е и освобождаются при дропе AS.
pub fn load_user_image(
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
    use std::{sync::Mutex, vec};

    use memory::{
        memory_mapper::{
            AddressSpaceTag, MemoryMapper, MemoryMappingError, MemoryRemappingError,
            MemoryUnmappingError,
        },
        physical_address::{PageAlignedAddress, PhysicalAddress},
        virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
    };

    use super::*;
    use crate::image::UserSegment;

    const PAGE: usize = FRAME_SIZE;

    fn aligned(addr: usize) -> PageAlignedVirtualAddress {
        PageAlignedVirtualAddress::from_usize(addr).expect("aligned addr in test")
    }

    fn rx_segment(va: usize, size: usize, init: &[u8]) -> UserSegment<'_> {
        UserSegment {
            va_base: aligned(va),
            mapped_size: size,
            init_bytes: init,
            perms: MemFlags::user_rx(),
        }
    }

    fn rw_segment(va: usize, size: usize, init: &[u8]) -> UserSegment<'_> {
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

    #[derive(Debug, PartialEq, Eq, Clone)]
    struct MapCall {
        va: usize,
        page_count: usize,
        init: std::vec::Vec<u8>,
    }

    struct MockMapper {
        calls: Mutex<std::vec::Vec<MapCall>>,
        fail_on_call: Option<usize>,
    }

    impl MockMapper {
        fn new() -> Self {
            Self {
                calls: Mutex::new(std::vec::Vec::new()),
                fail_on_call: None,
            }
        }
        fn failing_on(call_index: usize) -> Self {
            Self {
                calls: Mutex::new(std::vec::Vec::new()),
                fail_on_call: Some(call_index),
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
            let mut calls = self.calls.lock().unwrap();
            if self.fail_on_call == Some(calls.len()) {
                return Err(MemoryMappingError::OutOfMemory);
            }
            calls.push(MapCall {
                va: va.as_usize(),
                page_count,
                init: init.to_vec(),
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
                PhysicalAddress::new(0xDEAD_BEEF),
                AddressSpaceTag::NONE,
            )
        }
        fn zero_owned_frame(&self, _pa: memory::physical_address::PageAlignedAddress) {}
        fn as_any(&self) -> &(dyn core::any::Any + 'static) {
            self
        }
    }

    #[test]
    fn load_user_image_maps_segments_and_stack_in_order() {
        let init = vec![0xCDu8; 100];
        let segs = [rx_segment(0x4000_0000, PAGE, &init)];
        let image = make_image(&segs, 0x4000_0000);

        let mapper = MockMapper::new();

        load_user_image(&mapper, &image).expect("load ok");

        let calls = mapper.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].va, 0x4000_0000);
        assert_eq!(calls[0].page_count, 1);
        assert_eq!(calls[0].init.len(), 100);

        let stack_base = 0x1_0000_0000 - 4 * PAGE;
        assert_eq!(calls[1].va, stack_base);
        assert_eq!(calls[1].page_count, 4);
        assert_eq!(calls[1].init.len(), 0);
    }

    #[test]
    fn load_user_image_passes_init_bytes_to_mapper() {
        let init = vec![0xABu8; 8];
        let segs = [rx_segment(0x4000_0000, PAGE, &init)];
        let image = make_image(&segs, 0x4000_0000);

        let mapper = MockMapper::new();
        load_user_image(&mapper, &image).unwrap();

        let calls = mapper.calls();
        assert_eq!(calls[0].init, init);
    }

    #[test]
    fn load_user_image_propagates_mapper_error_on_partial_map() {
        let segs = [
            rx_segment(0x4000_0000, PAGE, &[]),
            rw_segment(0x4000_0000 + PAGE, PAGE, &[]),
        ];
        let image = make_image(&segs, 0x4000_0000);
        let mapper = MockMapper::failing_on(1);

        assert_eq!(
            load_user_image(&mapper, &image),
            Err(UserImageError::Mapping(MemoryMappingError::OutOfMemory))
        );
        let calls = mapper.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].va, 0x4000_0000);
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
