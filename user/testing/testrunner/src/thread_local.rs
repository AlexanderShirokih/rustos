//! Переменные `#[thread_local]` из EL0. У каждого потока свой экземпляр с начальным значением
//! программы и выравниванием `PT_TLS`, а память блока порождённого потока возвращается в бюджет
//! с присоединением потока.

use core::{cell::Cell, hint::black_box};

use runtime::spawn;

use crate::{
    fixture::{FIXTURE_ENTRY, Fixture, JOIN_TIMEOUT, process_bytes, program},
    fixture_protocol::{OP_EXIT, OP_JOIN_THREAD, REPLY_OK},
    resource_split::spare_pages,
};

const INITIAL: u64 = 0x7715_5EED;
const MAIN_VALUE: u64 = 0xA11;
const BLOCK_ALIGN: usize = 64;
const SEQUENTIAL_THREADS: usize = 8;
const FIXTURE_SLACK_BYTES: u64 = 256 * 1024;

#[repr(align(64))]
struct Aligned(Cell<u8>);

#[thread_local]
static INITIALIZED: Cell<u64> = Cell::new(INITIAL);

#[thread_local]
static ZEROED: Cell<u64> = Cell::new(0);

#[thread_local]
static ALIGNED: Aligned = Aligned(Cell::new(0));

fn aligned() -> bool {
    ALIGNED.0.set(black_box(1));
    let touched = black_box(ALIGNED.0.get()) == 1;

    touched
        && black_box(&raw const ALIGNED)
            .addr()
            .is_multiple_of(BLOCK_ALIGN)
}

extern "C" fn fresh_thread(_arg: usize) -> u32 {
    let fresh = INITIALIZED.get() == INITIAL && ZEROED.get() == 0 && aligned();
    INITIALIZED.set(MAIN_VALUE + 1);
    ZEROED.set(MAIN_VALUE + 1);

    u32::from(!fresh)
}

#[test_harness::test]
fn a_spawned_thread_starts_from_the_program_values() {
    INITIALIZED.set(MAIN_VALUE);
    ZEROED.set(MAIN_VALUE);

    let thread = spawn(fresh_thread, 0).expect("spawn thread");
    thread
        .wait(JOIN_TIMEOUT)
        .expect("thread exits before timeout");
    let fresh = thread.exit_code().expect("thread exit code") == 0;
    thread.join(JOIN_TIMEOUT).expect("thread joins");

    let kept = INITIALIZED.get() == MAIN_VALUE && ZEROED.get() == MAIN_VALUE;
    INITIALIZED.set(INITIAL);
    ZEROED.set(0);
    test_harness::check!(fresh);
    test_harness::check!(kept);
}

#[test_harness::test]
fn a_thread_local_keeps_its_alignment_in_the_main_thread() {
    test_harness::check!(aligned());
}

#[test_harness::test]
fn joined_threads_return_their_thread_local_blocks() {
    let budget = process_bytes(&program(FIXTURE_ENTRY)) + FIXTURE_SLACK_BYTES;
    let fixture = Fixture::spawn_split(budget, None).expect("spawn fixture");
    // Первый запуск потока расширяет кучу фикстуры для передачи тела потока.
    test_harness::check_eq!(fixture.request_status(OP_JOIN_THREAD, 0, &[]), REPLY_OK);

    let spare = spare_pages(&fixture);
    for _ in 0..SEQUENTIAL_THREADS {
        test_harness::check_eq!(fixture.request_status(OP_JOIN_THREAD, 0, &[]), REPLY_OK);
        test_harness::check_eq!(spare_pages(&fixture), spare);
    }
    test_harness::check_eq!(fixture.request_status(OP_EXIT, 0, &[]), REPLY_OK);
}
