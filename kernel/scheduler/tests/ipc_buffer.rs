//! `attach_ipc_buffer` (`scheduler.rs:1116`) + `set_frame_allocator`.
//! Покрываем три наблюдаемых контракта:
//!  - без зарегистрированного `FrameAllocator` user-поток создаётся без буфера
//!    (user_vm-регионы не выделяются);
//!  - с allocator-ом создаётся ровно один backing-регион IPC-буфера;
//!  - на ошибке `install` маппинга allocation откатывается, поток не создаётся.

mod common;

use std::sync::Arc;

use kobject::{KernelRuntime, UserThreadEntry};
use memory::virtual_address::{PageAlignedVirtualAddress, VirtualAddress};
use scheduler::{Scheduler, SchedulerConfig, Uninit};

use crate::common::{
    CountingFrameAllocator, MockAddressSpaceFactory, MockContext, MockTimer, MockTimerSource,
    reset_switches,
};

type TestScheduler = Scheduler<MockContext, MockTimerSource, Uninit>;
const TEST_CONFIG: SchedulerConfig = SchedulerConfig::new(32, 16);

fn factory_static(factory: MockAddressSpaceFactory) -> &'static MockAddressSpaceFactory {
    Box::leak(Box::new(factory))
}

fn frame_allocator_static() -> &'static CountingFrameAllocator {
    Box::leak(Box::new(CountingFrameAllocator::new()))
}

fn mark_loaded(handle: &impl KernelRuntime, process: &Arc<kobject::ProcessObject>) {
    let install = kobject::UserImageInstall {
        segments: std::vec::Vec::new(),
        entry: VirtualAddress::new(0x4000_0000),
        user_stack_top: VirtualAddress::new(0x5000_1000),
        user_stack_size: 0x1000,
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
    let factory = factory_static(MockAddressSpaceFactory::new());
    let timer = MockTimer::new();
    let scheduler = TestScheduler::with_address_space_factory(
        MockTimerSource(timer),
        TEST_CONFIG,
        Some(factory),
    )
    .bootstrap();
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
    let factory = factory_static(MockAddressSpaceFactory::new());
    let timer = MockTimer::new();
    let scheduler = TestScheduler::with_address_space_factory(
        MockTimerSource(timer),
        TEST_CONFIG,
        Some(factory),
    )
    .bootstrap();
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
fn terminate_thread_releases_its_ipc_buffer_region() {
    reset_switches();
    let factory = factory_static(MockAddressSpaceFactory::new());
    let timer = MockTimer::new();
    let scheduler = TestScheduler::with_address_space_factory(
        MockTimerSource(timer),
        TEST_CONFIG,
        Some(factory),
    )
    .bootstrap();
    scheduler.set_frame_allocator(frame_allocator_static());
    let handle = scheduler.handle();

    let process = handle.create_empty_process("p").expect("create process");
    mark_loaded(&handle, &process);
    let pid = scheduler.process_id_for(&process).expect("pid lookup");

    let thread_ko = handle
        .create_user_thread(&process, user_entry())
        .expect("create_user_thread");
    assert_eq!(scheduler.process_user_vm_region_count(pid), Some(1));

    // Терминация должна снять PTE и вернуть range IPC-буфера в user_vm -
    // иначе регион висел бы до сноса AS (регрессия на утечку фрейма).
    handle.terminate_thread(&thread_ko, 0).expect("terminate");
    assert_eq!(scheduler.process_user_vm_region_count(pid), Some(0));
}

#[test]
fn ipc_buffer_install_failure_rolls_back_allocation_and_thread() {
    reset_switches();
    let factory = factory_static(MockAddressSpaceFactory::with_failing_map_exact());
    let timer = MockTimer::new();
    let scheduler = TestScheduler::with_address_space_factory(
        MockTimerSource(timer),
        TEST_CONFIG,
        Some(factory),
    )
    .bootstrap();
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
