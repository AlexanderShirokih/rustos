//! Копирование и заполнение обычной памяти при любом смещении начал и любой длине: процедуры
//! `memcpy`, `memmove`, `memset` и операции маппинга. Каждая проверка сверяет диапазон с эталоном
//! побайтной копии и байты вокруг диапазона с исходным заполнением.

use alloc::{vec, vec::Vec};
use core::ffi::{c_int, c_void};

use kernel_objects::{Mapping, Resource};
use runtime::{MemoryAccess, UserMemFlags};

const PAGE_SIZE: u64 = syscall::PAGE_SIZE as u64;

const SHIFTS: core::ops::Range<usize> = 0..16;

const SHORT_LENS: core::ops::RangeInclusive<usize> = 0..=40;

const EDGE_LENS: [usize; 19] = [
    47, 48, 63, 64, 65, 95, 96, 97, 127, 128, 129, 159, 160, 161, 255, 256, 257, 1023, 4099,
];

const GUARD: usize = 32;

const UNTOUCHED: u8 = 0xC3;

unsafe extern "C" {
    fn memcpy(destination: *mut c_void, source: *const c_void, count: usize) -> *mut c_void;
    fn memmove(destination: *mut c_void, source: *const c_void, count: usize) -> *mut c_void;
    fn memset(destination: *mut c_void, value: c_int, count: usize) -> *mut c_void;
}

struct Arena {
    bytes: Vec<u8>,
    base: usize,
}

impl Arena {
    fn new(len: usize) -> Self {
        let bytes = vec![UNTOUCHED; len + 2 * GUARD + 64];
        let base = bytes.as_ptr().align_offset(64) + GUARD;
        Self { bytes, base }
    }

    fn at(&mut self, shift: usize) -> *mut u8 {
        self.bytes.as_mut_ptr().wrapping_add(self.base + shift)
    }

    fn window(&self, shift: usize, len: usize) -> &[u8] {
        &self.bytes[self.base + shift - GUARD..self.base + shift + len + GUARD]
    }

    fn reset(&mut self) {
        self.bytes.fill(UNTOUCHED);
    }
}

fn lens() -> impl Iterator<Item = usize> {
    SHORT_LENS.chain(EDGE_LENS)
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    let mut byte = seed;
    (0..len)
        .map(|_| {
            byte = byte.wrapping_mul(31).wrapping_add(7);
            byte
        })
        .collect()
}

fn guarded(content: &[u8]) -> Vec<u8> {
    let mut expected = vec![UNTOUCHED; content.len() + 2 * GUARD];
    expected[GUARD..GUARD + content.len()].copy_from_slice(content);
    expected
}

fn shifts_for(len: usize) -> impl Iterator<Item = usize> {
    let step = if len > 256 { 5 } else { 1 };
    SHIFTS.step_by(step)
}

fn mapped(pages: u64) -> Mapping {
    let resource = Resource::self_resource().expect("self metering resource");
    let region = resource
        .create_virtual(pages * PAGE_SIZE, MemoryAccess::RW)
        .expect("create region");
    Mapping::full(region, UserMemFlags::ReadWrite).expect("map region")
}

fn read_mapping(mapping: &Mapping, offset: usize, len: usize) -> Vec<u8> {
    let base = usize::try_from(mapping.va()).expect("va fits usize");
    (0..len)
        .map(|index| {
            let address = (base + offset + index) as *const u8;
            // SAFETY: адрес лежит в отображении `mapping`, которое живо на время вызова.
            unsafe { address.read_volatile() }
        })
        .collect()
}

fn fill_mapping(mapping: &Mapping, len: usize) {
    let base = usize::try_from(mapping.va()).expect("va fits usize");
    for index in 0..len + 2 * GUARD + SHIFTS.end {
        let address = (base + index) as *mut u8;
        // SAFETY: адрес лежит в отображении `mapping` на запись, которое живо на время вызова.
        unsafe { address.write_volatile(UNTOUCHED) };
    }
}

#[test_harness::test]
fn memcpy_copies_every_length_between_any_shifts() {
    for len in lens() {
        let content = pattern(len, 1);
        let mut source = Arena::new(len);
        let mut target = Arena::new(len);
        for source_shift in shifts_for(len) {
            for target_shift in shifts_for(len) {
                source.reset();
                target.reset();
                let from = source.at(source_shift);
                // SAFETY: обе области лежат внутри арен с запасом GUARD по краям.
                unsafe {
                    core::ptr::copy_nonoverlapping(content.as_ptr(), from, len);
                    memcpy(target.at(target_shift).cast(), from.cast(), len);
                }
                test_harness::check_eq!(target.window(target_shift, len), &guarded(&content)[..]);
            }
        }
    }
}

#[test_harness::test]
fn memmove_handles_overlap_in_both_directions() {
    const DISTANCES: [isize; 10] = [-129, -64, -17, -8, -1, 1, 8, 17, 64, 129];
    for len in lens() {
        let content = pattern(len, 2);
        let mut arena = Arena::new(len + 2 * 129);
        for distance in DISTANCES {
            for shift in shifts_for(len) {
                arena.reset();
                let source_shift = 129 + shift;
                let target_shift = source_shift.wrapping_add_signed(distance);
                let from = arena.at(source_shift);
                // SAFETY: оба диапазона лежат внутри арены с запасом GUARD по краям.
                unsafe {
                    core::ptr::copy_nonoverlapping(content.as_ptr(), from, len);
                    memmove(arena.at(target_shift).cast(), from.cast(), len);
                }
                let moved = arena.window(target_shift, len);
                test_harness::check_eq!(&moved[GUARD..GUARD + len], &content[..]);
            }
        }
    }
}

#[test_harness::test]
fn memset_fills_every_length_at_any_shift() {
    for value in [0u8, 0xA5] {
        for len in lens() {
            let expected = guarded(&vec![value; len]);
            let mut arena = Arena::new(len);
            for shift in shifts_for(len) {
                arena.reset();
                // SAFETY: диапазон лежит внутри арены с запасом GUARD по краям.
                unsafe { memset(arena.at(shift).cast(), c_int::from(value), len) };
                test_harness::check_eq!(arena.window(shift, len), &expected[..]);
            }
        }
    }
}

#[test_harness::test]
fn mapping_copy_in_and_out_are_exact_at_any_shift() {
    let mapping = mapped(2);
    for len in lens() {
        let content = pattern(len, 3);
        for mapping_shift in shifts_for(len) {
            for buffer_shift in shifts_for(len) {
                fill_mapping(&mapping, len);
                let mut source = Arena::new(len);
                // SAFETY: диапазон лежит внутри арены с запасом GUARD по краям.
                unsafe {
                    core::ptr::copy_nonoverlapping(content.as_ptr(), source.at(buffer_shift), len);
                }
                let staged = &source.window(buffer_shift, len)[GUARD..GUARD + len];
                mapping
                    .copy_in_at(GUARD + mapping_shift, staged)
                    .expect("copy into mapping");
                test_harness::check_eq!(
                    read_mapping(&mapping, mapping_shift, len + 2 * GUARD),
                    guarded(&content)
                );

                let mut target = Arena::new(len);
                let start = target.base + buffer_shift;
                mapping
                    .copy_out_at(GUARD + mapping_shift, &mut target.bytes[start..start + len])
                    .expect("copy out of mapping");
                test_harness::check_eq!(target.window(buffer_shift, len), &guarded(&content)[..]);
            }
        }
    }
}

#[test_harness::test]
fn mapping_copy_to_and_zero_are_exact_at_any_shift() {
    let source = mapped(2);
    let target = mapped(2);
    for len in lens() {
        let content = pattern(len, 4);
        for source_shift in shifts_for(len) {
            for target_shift in shifts_for(len) {
                fill_mapping(&source, len);
                fill_mapping(&target, len);
                source
                    .copy_in_at(GUARD + source_shift, &content)
                    .expect("stage source");
                source
                    .copy_to(GUARD + source_shift, &target, GUARD + target_shift, len)
                    .expect("copy between mappings");
                test_harness::check_eq!(
                    read_mapping(&target, target_shift, len + 2 * GUARD),
                    guarded(&content)
                );

                target
                    .zero_at(GUARD + target_shift, len)
                    .expect("zero mapping range");
                test_harness::check_eq!(
                    read_mapping(&target, target_shift, len + 2 * GUARD),
                    guarded(&vec![0; len])
                );
            }
        }
    }
}
