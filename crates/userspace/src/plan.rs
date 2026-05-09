//! Подготовка per-process [`UserVmAllocator`] для свежесозданного user-процесса.

use memory::{user_vm_allocator::UserVmAllocator, virtual_address::PageAlignedVirtualAddress};

use crate::image::UserImage;

/// Готовит per-process [`UserVmAllocator`] для user-процесса.
///
/// Аллокатор обслуживает "дыру" между концом самого высокого сегмента образа и
/// базой user-стека: всё, что можно получить через будущие `vm_allocate` syscall'ы,
/// лежит именно здесь. Возвращает `None`, если такой дыры нет (например, образ
/// без сегментов или сегменты вплотную примыкают к стеку) - для таких процессов
/// `vm_*` syscall'ы будут всегда возвращать `OutOfMemory`.
pub fn build_user_vm_allocator(image: &UserImage<'_>) -> Option<UserVmAllocator> {
    let highest_end = image.highest_segment_end()?;
    let stack_base = image.user_stack_base().ok()?;
    let highest_aligned = PageAlignedVirtualAddress::from_usize(highest_end.as_usize())?;
    if highest_aligned.as_usize() >= stack_base.as_usize() {
        return None;
    }
    Some(UserVmAllocator::new(
        highest_aligned,
        stack_base.as_virtual(),
    ))
}

#[cfg(test)]
mod tests {
    use memory::{
        MemFlags,
        virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
    };

    use super::*;
    use crate::image::{UserImage, UserSegment};

    const PAGE: usize = 4096;
    const STACK_TOP: usize = 0x1_0000_0000;
    const STACK_SIZE: usize = 4 * PAGE;
    const STACK_BASE: usize = STACK_TOP - STACK_SIZE;

    fn aligned(addr: usize) -> PageAlignedVirtualAddress {
        PageAlignedVirtualAddress::from_usize(addr).expect("aligned addr")
    }

    fn rx_segment<'a>(va: usize, size: usize) -> UserSegment<'a> {
        UserSegment {
            va_base: aligned(va),
            mapped_size: size,
            init_bytes: &[],
            perms: MemFlags::user_rx(),
        }
    }

    fn make_image<'a>(segments: &'a [UserSegment<'a>]) -> UserImage<'a> {
        UserImage {
            segments,
            entry: VirtualAddress::new(0x4000_0000),
            user_stack_top: VirtualAddress::new(STACK_TOP),
            user_stack_size: STACK_SIZE,
        }
    }

    #[test]
    fn returns_none_when_no_segments() {
        let image = make_image(&[]);
        assert!(build_user_vm_allocator(&image).is_none());
    }

    #[test]
    fn returns_none_when_image_touches_stack() {
        // Сегмент примыкает к stack_base: highest_end == stack_base -> дыры нет.
        let segs = [rx_segment(STACK_BASE - PAGE, PAGE)];
        let image = make_image(&segs);
        assert!(
            build_user_vm_allocator(&image).is_none(),
            "no gap between image and stack must yield None"
        );
    }

    #[test]
    fn returns_alloc_with_expected_bounds() {
        // Сегмент [0x4000_0000, 0x4000_1000); stack_base = STACK_TOP - 4*PAGE.
        // Дыра: [0x4000_1000, stack_base).
        let seg_va = 0x4000_0000;
        let segs = [rx_segment(seg_va, PAGE)];
        let image = make_image(&segs);

        let alloc = build_user_vm_allocator(&image).expect("non-empty gap must produce allocator");
        // Свежий аллокатор пуст: нет ни одного выделенного региона.
        assert_eq!(alloc.live_count(), 0);
        // Границы: нижняя - конец самого высокого сегмента, верхняя - base стека.
        assert_eq!(alloc.region_start().as_usize(), seg_va + PAGE);
        assert_eq!(alloc.region_end().as_usize(), STACK_BASE);
    }

    #[test]
    fn returns_none_when_segments_above_stack() {
        // Patological-кейс: сегмент стартует выше user_stack_top. Validate
        // его отвергнул бы (StackOverlapsSegment), но build_user_vm_allocator
        // не выполняет полную валидацию - он лишь обязан не падать и вернуть
        // None для случая `highest_aligned >= stack_base`.
        let segs = [rx_segment(STACK_TOP + PAGE, PAGE)];
        let image = make_image(&segs);
        assert!(
            build_user_vm_allocator(&image).is_none(),
            "highest_aligned >= stack_base must yield None"
        );
    }
}
