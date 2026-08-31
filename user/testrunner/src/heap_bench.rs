//! Замеры кучи процесса: цена пары alloc/free по размерам и цена оборота живых блоков.
//!
//! Прогон идёт под TCG, где нет модели кеша: сравнимы замеры между собой. Размеры покрывают
//! обе ветки кучи - обслуживаемую списками свободных и уходящую к ядру.

use alloc::alloc::{Layout, alloc, dealloc};
use core::hint::black_box;

use kernel_tests::{kernel_test, kreport};
use runtime::Instant;

const SIZES: [usize; 6] = [32, 256, 2048, 4096, 16 * 1024, 64 * 1024];

const LIVE: usize = 16;

fn rounds_for(size: usize) -> u64 {
    match size {
        0..=2048 => 512,
        2049..=16384 => 128,
        _ => 32,
    }
}

fn nanos_per_op(rounds: u64, mut operation: impl FnMut()) -> u64 {
    let start = Instant::now();
    for _ in 0..rounds {
        operation();
    }
    let elapsed = Instant::now().as_nanos().saturating_sub(start.as_nanos());
    elapsed / rounds
}

fn layout_of(size: usize) -> Layout {
    Layout::from_size_align(size, 16).expect("layout of a bench size")
}

#[kernel_test(bench)]
fn bench_heap_alloc_free() {
    for size in SIZES {
        let layout = layout_of(size);
        let elapsed = nanos_per_op(rounds_for(size), || {
            // SAFETY: размер ненулевой, выравнивание - степень двойки; блок освобождается
            // тем же layout и больше не адресуется.
            unsafe {
                let block = alloc(layout);
                assert!(!block.is_null(), "heap block");
                black_box(block.read_volatile());
                dealloc(block, layout);
            }
        });
        kreport!("heap alloc_free bytes={size} ns_per_op={elapsed}");
    }
}

#[kernel_test(bench)]
fn bench_heap_live_set() {
    // Оборот живых блоков: куча держит LIVE занятых и отдаёт по одному на круг, поэтому
    // замер видит переиспользование списка свободных, а не первую выдачу.
    for size in SIZES {
        let layout = layout_of(size);
        let mut live = [core::ptr::null_mut(); LIVE];
        for slot in &mut live {
            // SAFETY: см. bench_heap_alloc_free; блоки живут до конца замера.
            *slot = unsafe { alloc(layout) };
            assert!(!slot.is_null(), "heap block");
        }

        let mut cursor = 0;
        let elapsed = nanos_per_op(rounds_for(size), || {
            // SAFETY: слот несёт блок этого же layout, выданный выше либо кругом раньше.
            unsafe {
                dealloc(live[cursor], layout);
                let block = alloc(layout);
                assert!(!block.is_null(), "heap block");
                live[cursor] = block;
            }
            cursor = (cursor + 1) % LIVE;
        });
        kreport!("heap live_set bytes={size} ns_per_op={elapsed}");

        for slot in live {
            // SAFETY: блок выдан этим же layout и больше не адресуется.
            unsafe { dealloc(slot, layout) };
        }
    }
}
