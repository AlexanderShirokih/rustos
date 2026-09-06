//! Область, нарезанная per-CPU фреймами: поиск фрейма по affinity владельца.

use alloc::vec::Vec;

use crate::ipi_affinity::IpiAffinity;

/// Владелец фрейма и признак того, что фрейм последний в области.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameOwner {
    pub affinity: IpiAffinity,
    pub last: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Frame {
    affinity: IpiAffinity,
    offset: usize,
}

/// Фреймы отображённой области с affinity их владельцев.
///
/// Соответствие задано железом. Значение снимается один раз и дальше только
/// читается.
pub struct AffinityFrames {
    frames: Vec<Frame>,
}

impl AffinityFrames {
    /// Снимает соответствие с железа, обходя фреймы с шагом `stride` от базы
    /// области и спрашивая владельца каждого через `read_owner`.
    ///
    /// Обход кончается на фрейме, отмеченном последним, либо на первом
    /// недоступном - для такого `read_owner` возвращает `None`.
    pub fn scan(stride: usize, mut read_owner: impl FnMut(usize) -> Option<FrameOwner>) -> Self {
        let mut frames = Vec::new();
        let mut offset = 0usize;

        while let Some(owner) = read_owner(offset) {
            frames.push(Frame {
                affinity: owner.affinity,
                offset,
            });

            if owner.last {
                break;
            }

            let Some(next) = offset.checked_add(stride).filter(|next| *next > offset) else {
                break;
            };
            offset = next;
        }

        Self { frames }
    }

    /// Смещение фрейма, принадлежащего CPU с этим affinity.
    pub fn offset_of(&self, affinity: IpiAffinity) -> Option<usize> {
        self.frames
            .iter()
            .find(|frame| frame.affinity == affinity)
            .map(|frame| frame.offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STRIDE: usize = 0x2_0000;

    fn owner(aff3: u8, aff2: u8, aff1: u8, aff0: u8, last: bool) -> FrameOwner {
        FrameOwner {
            affinity: affinity(aff3, aff2, aff1, aff0),
            last,
        }
    }

    fn affinity(aff3: u8, aff2: u8, aff1: u8, aff0: u8) -> IpiAffinity {
        IpiAffinity {
            aff0,
            aff1,
            aff2,
            aff3,
        }
    }

    fn region(owners: &[FrameOwner]) -> AffinityFrames {
        AffinityFrames::scan(STRIDE, |offset| owners.get(offset / STRIDE).copied())
    }

    #[test]
    fn frame_is_found_by_the_affinity_of_its_owner() {
        let frames = region(&[
            owner(0, 0, 0, 0, false),
            owner(0, 0, 0, 1, false),
            owner(0, 0, 0, 2, true),
        ]);

        assert_eq!(frames.offset_of(affinity(0, 0, 0, 0)), Some(0));
        assert_eq!(frames.offset_of(affinity(0, 0, 0, 1)), Some(STRIDE));
        assert_eq!(frames.offset_of(affinity(0, 0, 0, 2)), Some(2 * STRIDE));
    }

    #[test]
    fn owners_differing_at_any_level_are_told_apart() {
        let frames = region(&[owner(4, 3, 2, 1, true)]);

        assert_eq!(frames.offset_of(affinity(4, 3, 2, 1)), Some(0));
        assert_eq!(frames.offset_of(affinity(0, 3, 2, 1)), None);
        assert_eq!(frames.offset_of(affinity(4, 3, 2, 0)), None);
    }

    #[test]
    fn frame_order_does_not_follow_the_affinity_order() {
        let frames = region(&[
            owner(0, 0, 1, 7, false),
            owner(0, 0, 0, 3, false),
            owner(0, 0, 1, 0, true),
        ]);

        assert_eq!(frames.offset_of(affinity(0, 0, 1, 7)), Some(0));
        assert_eq!(frames.offset_of(affinity(0, 0, 0, 3)), Some(STRIDE));
    }

    #[test]
    fn affinity_without_a_frame_of_its_own_is_not_found() {
        let frames = region(&[owner(0, 0, 0, 0, true)]);

        assert_eq!(frames.offset_of(affinity(0, 0, 0, 1)), None);
    }

    #[test]
    fn region_without_a_readable_frame_stays_empty() {
        let frames = AffinityFrames::scan(STRIDE, |_| None);

        assert_eq!(frames.offset_of(affinity(0, 0, 0, 0)), None);
    }

    #[test]
    fn frames_past_the_last_one_are_not_taken() {
        let frames = region(&[
            owner(0, 0, 0, 0, true),
            owner(0, 0, 0, 1, false),
            owner(0, 0, 0, 2, true),
        ]);

        assert_eq!(frames.offset_of(affinity(0, 0, 0, 0)), Some(0));
        assert_eq!(frames.offset_of(affinity(0, 0, 0, 1)), None);
        assert_eq!(frames.offset_of(affinity(0, 0, 0, 2)), None);
    }

    #[test]
    fn frames_past_the_readable_area_are_not_taken() {
        let frames = region(&[owner(0, 0, 0, 0, false), owner(0, 0, 0, 1, false)]);

        assert_eq!(frames.offset_of(affinity(0, 0, 0, 1)), Some(STRIDE));
        assert_eq!(frames.offset_of(affinity(0, 0, 0, 2)), None);
    }

    #[test]
    fn step_that_stays_on_the_same_frame_ends_the_scan() {
        let mut reads = 0usize;
        AffinityFrames::scan(0, |_| {
            reads += 1;
            Some(owner(0, 0, 0, 0, false))
        });

        assert_eq!(reads, 1);
    }

    #[test]
    fn step_past_the_address_space_ends_the_scan() {
        let mut reads = 0usize;
        AffinityFrames::scan(usize::MAX, |_| {
            reads += 1;
            Some(owner(0, 0, 0, 0, false))
        });

        assert_eq!(reads, 2);
    }
}
