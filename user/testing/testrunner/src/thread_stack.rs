//! Переполнение стека потока из EL0. Поток с малым стеком уходит в рекурсию, запись под
//! нижней границей стека попадает на защитную страницу, и процесс завершается отказом
//! доступа к памяти, не задев память ниже защитной страницы. Кадр больше страницы не
//! перешагивает защитную страницу.

use core::sync::atomic::Ordering::Relaxed;

use kernel_objects::Resource;
use runtime::{MemoryAccess, UserMemFlags};
use syscall::Rights;
use userland::USERLAND_PAGE_SIZE;

use crate::{
    fixture::{JOIN_TIMEOUT, spawn_entry},
    thread_stack_protocol::{
        JOIN_FAILED_EXIT_CODE, LARGE_FRAME_BYTES, MAP_FAILED_EXIT_CODE,
        RECURSION_RETURNED_EXIT_CODE, SMALL_FRAME_BYTES, SPAWN_FAILED_EXIT_CODE, StackReport,
        THREAD_STACK_BYTES, UNKNOWN_FRAME_EXIT_CODE,
    },
};

const OVERFLOW_FIXTURE: &str = "/stack-overflow-fixture/exec";
const DEEPEST_FRAME_SPAN: u64 = 512;

#[test_harness::test]
fn thread_stack_overflow_faults_on_the_guard_page() {
    overflow_lands_on_the_guard_page(SMALL_FRAME_BYTES);
}

#[test_harness::test]
fn thread_stack_overflow_with_large_frames_faults_on_the_guard_page() {
    overflow_lands_on_the_guard_page(LARGE_FRAME_BYTES);
}

fn overflow_lands_on_the_guard_page(frame_bytes: usize) {
    let frame_bytes = u64::try_from(frame_bytes).expect("frame size fits u64");
    let stack_bytes = u64::try_from(THREAD_STACK_BYTES).expect("stack size fits u64");

    let resource = Resource::self_resource().expect("self metering resource");
    let region = resource
        .create_virtual(USERLAND_PAGE_SIZE, MemoryAccess::RW)
        .expect("create report region");
    let mapping = region
        .map(USERLAND_PAGE_SIZE, UserMemFlags::ReadWrite)
        .expect("map report region");

    let va = usize::try_from(mapping.va()).expect("positive va fits usize");
    // SAFETY: map выдал RW-маппинг в страницу, и отчёт лежит в его начале. Фикстура пишет
    // отчёт атомарными операциями.
    let report = unsafe { &*(va as *const StackReport) };
    report.frame_bytes.store(frame_bytes, Relaxed);

    let lease = region
        .duplicate(Rights::READ | Rights::WRITE | Rights::TRANSFER, None)
        .expect("report region for the fixture");
    let process = spawn_entry(OVERFLOW_FIXTURE, lease);

    process.join(JOIN_TIMEOUT).expect("fixture terminates");
    let exit_code = process.exit_code().expect("exit code");
    test_harness::check!(
        ![
            0,
            MAP_FAILED_EXIT_CODE,
            SPAWN_FAILED_EXIT_CODE,
            RECURSION_RETURNED_EXIT_CODE,
            JOIN_FAILED_EXIT_CODE,
            UNKNOWN_FRAME_EXIT_CODE,
        ]
        .contains(&exit_code)
    );

    let entry = report.entry_frame.load(Relaxed);
    let deepest = report.deepest_frame.load(Relaxed);
    test_harness::check!(entry != 0);
    test_harness::check!(deepest != 0);

    // Фикстура не трогает буфер кадра, и буфер последнего кадра может лечь на защитную
    // страницу, но не ниже неё. Следующий кадр не поместился над нижней границей стека.
    let stack_base = (entry / USERLAND_PAGE_SIZE + 1) * USERLAND_PAGE_SIZE - stack_bytes;
    test_harness::check!(deepest >= stack_base - USERLAND_PAGE_SIZE);
    test_harness::check!(deepest < stack_base + frame_bytes + DEEPEST_FRAME_SPAN);
}
