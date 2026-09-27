//! Переполнение стека потока из EL0. Поток с малым стеком уходит в рекурсию, запись под
//! нижней границей стека попадает на защитную страницу, и процесс завершается отказом
//! доступа к памяти, не задев память ниже стека.

use core::sync::atomic::Ordering::Relaxed;

use kernel_objects::Resource;
use kernel_tests::kernel_test;
use runtime::{MemoryAccess, Timeout, UserMemFlags};
use syscall::Rights;
use userland_image::component_name;
use userland_loader::load_entry;

use crate::{
    fixture::program,
    thread_stack_protocol::{
        JOIN_FAILED_EXIT_CODE, MAP_FAILED_EXIT_CODE, RECURSION_RETURNED_EXIT_CODE,
        SPAWN_FAILED_EXIT_CODE, StackReport, THREAD_STACK_BYTES,
    },
};

const OVERFLOW_FIXTURE: &str = "/stack-overflow-fixture/program";
const JOIN_TIMEOUT: Timeout = Timeout::from_secs(5);
const PAGE_SIZE: u64 = syscall::PAGE_SIZE as u64;
const DEEPEST_FRAME_SPAN: u64 = 512;

#[kernel_test]
fn thread_stack_overflow_faults_on_the_guard_page() {
    let resource = Resource::self_resource().expect("self metering resource");
    let region = resource
        .create_virtual(PAGE_SIZE, MemoryAccess::RW)
        .expect("create report region");
    let mapping = region
        .map(PAGE_SIZE, UserMemFlags::ReadWrite)
        .expect("map report region");

    let lease = region
        .duplicate(Rights::READ | Rights::WRITE | Rights::TRANSFER, None)
        .expect("report region for the fixture");
    let process = load_entry(
        &program(OVERFLOW_FIXTURE),
        component_name(OVERFLOW_FIXTURE),
        &resource,
        lease,
    )
    .expect("spawn overflow fixture");

    process.join(JOIN_TIMEOUT).expect("fixture terminates");
    let exit_code = process.exit_code().expect("exit code");
    kernel_tests::kassert!(
        ![
            0,
            MAP_FAILED_EXIT_CODE,
            SPAWN_FAILED_EXIT_CODE,
            RECURSION_RETURNED_EXIT_CODE,
            JOIN_FAILED_EXIT_CODE,
        ]
        .contains(&exit_code)
    );

    let va = usize::try_from(mapping.va()).expect("positive va fits usize");
    // SAFETY: map выдал RW-маппинг в страницу, отчёт лежит в его начале, а фикстура, которая
    // писала отчёт, завершилась.
    let report = unsafe { &*(va as *const StackReport) };
    let entry = report.entry_frame.load(Relaxed);
    let deepest = report.deepest_frame.load(Relaxed);
    kernel_tests::kassert!(entry != 0);

    let stack_base = (entry / PAGE_SIZE + 1) * PAGE_SIZE - THREAD_STACK_BYTES;
    kernel_tests::kassert!(deepest >= stack_base);
    kernel_tests::kassert!(deepest < stack_base + DEEPEST_FRAME_SPAN);
}
