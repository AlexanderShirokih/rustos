//! `attach_ipc_buffer` (`scheduler.rs:1116`) + `set_frame_allocator`.
//! Покрываем три наблюдаемых контракта:
//!  - без зарегистрированного `FrameAllocator` user-поток создаётся без буфера
//!    (user_vm-регионы не выделяются);
//!  - с allocator-ом создаётся ровно один backing-регион IPC-буфера;
//!  - на ошибке `install` маппинга allocation откатывается, поток не создаётся;
//!  - заполненная таблица отображений отвергает поток `MappingTableFull` до выделения кадра.

#![allow(clippy::expect_used)]

mod common;

use std::sync::Arc;

use capability::{SpawnError, UserThreadEntry};
use memory::{
    testing::PageMapperFactory,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};
use scheduler::{Scheduler, SchedulerConfig, SchedulerHandle, Uninit};

use crate::common::{
    CountingFrameAllocator, MockContext, MockTimer, MockTimerSource, reset_switches,
};

type TestScheduler = Scheduler<MockContext, MockTimerSource, Uninit>;
const TEST_CONFIG: SchedulerConfig = SchedulerConfig::new(32, 16);

const MAPPING_TABLE_FILL_THREADS_MAX: usize =
    memory::range_allocator::DEFAULT_LIVE_REGION_CAPACITY + 2;

fn factory_static(factory: PageMapperFactory) -> &'static PageMapperFactory {
    Box::leak(Box::new(factory))
}

fn frame_allocator_static() -> &'static CountingFrameAllocator {
    Box::leak(Box::new(CountingFrameAllocator::new()))
}

fn mark_loaded(
    handle: &SchedulerHandle<MockContext, MockTimerSource>,
    process: &Arc<capability::ProcessObject>,
) {
    let install = capability::UserImageInstall {
        segments: std::vec::Vec::new(),
        entry: VirtualAddress::new(0x4000_0000),
        user_vm_base: PageAlignedVirtualAddress::from_usize(0x4000_0000).expect("aligned"),
        user_vm_size: 0x100_0000,
    };
    handle
        .load_user_image_into(process, &install)
        .expect("load stub image");
}

fn user_entry() -> UserThreadEntry {
    UserThreadEntry {
        entry_pc: 0x4000_0000,
        user_sp: 0x4001_0000,
        arg: 0,
        priority: 1,
    }
}

#[test]
fn without_frame_allocator_user_thread_has_no_ipc_buffer_region() {
    reset_switches();
    let factory = factory_static(PageMapperFactory::new());
    let timer = MockTimer::new();
    let scheduler = TestScheduler::with_address_space_factory(
        MockTimerSource(timer),
        TEST_CONFIG,
        Some(factory),
    )
    .bootstrap()
    .ok()
    .expect("bootstrap must succeed in tests");
    let handle = scheduler.handle();

    let process = handle.create_empty_process("p").expect("create process");
    mark_loaded(&handle, &process);
    let pid = scheduler.process_id_for(&process).expect("pid lookup");

    handle
        .create_user_thread(&process, user_entry())
        .expect("create_user_thread");

    assert_eq!(scheduler.process_user_vm_region_count(pid), Some(0));
}

#[test]
fn with_frame_allocator_user_thread_attaches_one_ipc_buffer_region() {
    reset_switches();
    let factory = factory_static(PageMapperFactory::new());
    let timer = MockTimer::new();
    let scheduler = TestScheduler::with_address_space_factory(
        MockTimerSource(timer),
        TEST_CONFIG,
        Some(factory),
    )
    .bootstrap()
    .ok()
    .expect("bootstrap must succeed in tests");
    scheduler.set_frame_allocator(frame_allocator_static());
    let handle = scheduler.handle();

    let process = handle.create_empty_process("p").expect("create process");
    mark_loaded(&handle, &process);
    let pid = scheduler.process_id_for(&process).expect("pid lookup");
    assert_eq!(scheduler.process_user_vm_region_count(pid), Some(0));

    handle
        .create_user_thread(&process, user_entry())
        .expect("create_user_thread");

    assert_eq!(scheduler.process_user_vm_region_count(pid), Some(1));
}

#[test]
fn thread_exiting_at_its_boundary_releases_its_ipc_buffer_region() {
    reset_switches();
    let factory = factory_static(PageMapperFactory::new());
    let timer = MockTimer::new();
    let scheduler = TestScheduler::with_address_space_factory(
        MockTimerSource(timer),
        TEST_CONFIG,
        Some(factory),
    )
    .bootstrap()
    .ok()
    .expect("bootstrap must succeed in tests");
    scheduler.set_frame_allocator(frame_allocator_static());
    let handle = scheduler.handle();

    let process = handle.create_empty_process("p").expect("create process");
    mark_loaded(&handle, &process);
    let pid = scheduler.process_id_for(&process).expect("pid lookup");

    // Второй поток держит процесс живым, пока первый выходит.
    let first = handle
        .create_user_thread(&process, user_entry())
        .expect("create first thread");
    handle
        .create_user_thread(&process, user_entry())
        .expect("create second thread");
    assert_eq!(scheduler.process_user_vm_region_count(pid), Some(2));

    let running = scheduler.run();
    assert!(Arc::ptr_eq(
        &handle.current_thread_object().expect("current"),
        &first
    ));
    handle.terminate_process(&process, 0).expect("terminate");
    assert_eq!(
        running.process_user_vm_region_count(pid),
        Some(2),
        "a thread keeps its IPC buffer until it reaches its boundary",
    );

    // Выход должен снять PTE и вернуть range IPC-буфера в user_vm - иначе регион висел бы
    // до сноса AS (регрессия на утечку фрейма).
    capability::KernelRuntime::exit_if_requested(&handle);
    assert_eq!(running.process_user_vm_region_count(pid), Some(1));
}

#[test]
fn ipc_buffer_install_failure_rolls_back_allocation_and_thread() {
    reset_switches();
    let factory = factory_static(PageMapperFactory::failing_map_exact());
    let timer = MockTimer::new();
    let scheduler = TestScheduler::with_address_space_factory(
        MockTimerSource(timer),
        TEST_CONFIG,
        Some(factory),
    )
    .bootstrap()
    .ok()
    .expect("bootstrap must succeed in tests");
    scheduler.set_frame_allocator(frame_allocator_static());
    let handle = scheduler.handle();

    let process = handle.create_empty_process("p").expect("create process");
    mark_loaded(&handle, &process);
    let pid = scheduler.process_id_for(&process).expect("pid lookup");

    let threads_before = scheduler.process_thread_count(pid);
    let err = handle.create_user_thread(&process, user_entry());
    assert!(
        err.is_err(),
        "create_user_thread must fail when IPC-buffer install fails"
    );

    assert_eq!(scheduler.process_user_vm_region_count(pid), Some(0));
    assert_eq!(scheduler.process_thread_count(pid), threads_before);
}

#[test]
fn full_mapping_table_refuses_thread_before_allocating_a_frame() {
    reset_switches();
    let factory = factory_static(PageMapperFactory::new());
    let timer = MockTimer::new();
    let scheduler = TestScheduler::with_address_space_factory(
        MockTimerSource(timer),
        SchedulerConfig::new(32, MAPPING_TABLE_FILL_THREADS_MAX),
        Some(factory),
    )
    .bootstrap()
    .ok()
    .expect("bootstrap must succeed in tests");
    let frames = frame_allocator_static();
    scheduler.set_frame_allocator(frames);
    let handle = scheduler.handle();

    let process = handle.create_empty_process("p").expect("create process");
    mark_loaded(&handle, &process);
    let refusal = (0..MAPPING_TABLE_FILL_THREADS_MAX)
        .find_map(|_| handle.create_user_thread(&process, user_entry()).err());
    assert_eq!(refusal, Some(SpawnError::MappingTableFull));

    let frames_before = frames.allocated_count();
    let err = handle.create_user_thread(&process, user_entry());
    assert_eq!(err.err(), Some(SpawnError::MappingTableFull));
    assert_eq!(frames.allocated_count(), frames_before);
}
