//! Host-тесты на интеграцию userspace-процессов в scheduler.
//!
//! Все тесты используют `MockContext`/`MockTimerSource` из `common.rs` плюс
//! mock-фабрику AS и mock-аллокатор фреймов: `Scheduler::spawn_user_process`
//! проходит реальный путь (валидация, `load_user_image`, регистрация
//! Process+Thread, ready-queue) без выхода в user-mode.

mod common;

use std::boxed::Box;

use kernelspace::{SpawnUserError, UserProcessSpawner};
use kobject::{Event, Handle, KObject, Rights};
use memory::{
    MemFlags,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};
use scheduler::{
    Priority, Scheduler, SchedulerConfig, SchedulerService, SchedulerServiceExt, SpawnAddressSpace,
    SpawnConfig, Uninit, UserProcessLaunch,
};
use userspace::{UserImage, UserImageError, UserSegment};

use crate::common::{
    MockAddressSpaceFactory, MockContext, MockTimer, MockTimerSource, reset_switches,
    take_address_space_switches,
};

const TEST_CONFIG: SchedulerConfig = SchedulerConfig::new(8, 16);
const PAGE: usize = 4096;
const USER_SEGMENT_VA: usize = 0x4000_0000;
const USER_STACK_TOP_VA: usize = 0x1_0000_0000;
const USER_STACK_SIZE: usize = 4 * PAGE;

type TestScheduler = Scheduler<MockContext, MockTimerSource, Uninit>;
type BootstrappedScheduler = Scheduler<MockContext, MockTimerSource, scheduler::Bootstrapped>;

fn aligned(addr: usize) -> PageAlignedVirtualAddress {
    PageAlignedVirtualAddress::from_usize(addr).expect("aligned addr")
}

fn make_scheduler(
    timer: std::sync::Arc<MockTimer>,
    factory: &'static MockAddressSpaceFactory,
) -> BootstrappedScheduler {
    TestScheduler::with_address_space_factory(MockTimerSource(timer), TEST_CONFIG, Some(factory))
        .bootstrap()
}

fn fresh_factory() -> &'static MockAddressSpaceFactory {
    Box::leak(Box::new(MockAddressSpaceFactory::new()))
}

#[test]
fn spawn_user_process_creates_address_space_and_thread() {
    reset_switches();
    let factory = fresh_factory();
    let timer = MockTimer::new();
    let scheduler = make_scheduler(timer, factory);

    let init = [0xAAu8; 8];
    let segments = [UserSegment {
        va_base: aligned(USER_SEGMENT_VA),
        mapped_size: PAGE,
        init_bytes: &init,
        perms: MemFlags::user_rx(),
    }];
    let image = UserImage {
        segments: &segments,
        entry: VirtualAddress::new(USER_SEGMENT_VA),
        user_stack_top: VirtualAddress::new(USER_STACK_TOP_VA),
        user_stack_size: USER_STACK_SIZE,
    };

    let (_pid, _tid) = scheduler
        .spawn_user_process("user-a", &image, Priority::new(2), 4)
        .expect("spawn user process");

    assert_eq!(factory.created(), 1);
    // process_count: idle + user = 2.
    assert_eq!(scheduler.process_count(), 2);
    // 1 map() для сегмента + 1 map() для user-stack = 2 вызова.
    let calls = factory.map_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].page_count, 1);
    assert_eq!(calls[1].page_count, USER_STACK_SIZE / PAGE);
}

#[test]
fn spawn_user_process_with_launch_installs_initial_handles() {
    reset_switches();
    let factory = fresh_factory();
    let timer = MockTimer::new();
    let scheduler = make_scheduler(timer, factory);

    let init = [0xAAu8; 8];
    let segments = [UserSegment {
        va_base: aligned(USER_SEGMENT_VA),
        mapped_size: PAGE,
        init_bytes: &init,
        perms: MemFlags::user_rx(),
    }];
    let image = UserImage {
        segments: &segments,
        entry: VirtualAddress::new(USER_SEGMENT_VA),
        user_stack_top: VirtualAddress::new(USER_STACK_TOP_VA),
        user_stack_size: USER_STACK_SIZE,
    };

    let handle = Handle::new(KObject::Event(Event::new()), Rights::SIGNAL);
    let launch = UserProcessLaunch::new()
        .initial_handles(vec![handle])
        .bootstrap_handle(0);
    let info = scheduler
        .spawn_user_process_with_launch("user-launch", &image, Priority::new(2), 4, launch)
        .expect("spawn user process with launch options");

    assert_eq!(info.initial_handle_ids.len(), 1);
    assert_eq!(info.initial_handle_ids[0].raw().get(), 1 << 16);
    assert_eq!(scheduler.process_count(), 2);
}

#[test]
fn spawn_user_process_with_launch_rejects_bad_bootstrap_handle_index() {
    reset_switches();
    let factory = fresh_factory();
    let timer = MockTimer::new();
    let scheduler = make_scheduler(timer, factory);

    let init = [0xAAu8; 8];
    let segments = [UserSegment {
        va_base: aligned(USER_SEGMENT_VA),
        mapped_size: PAGE,
        init_bytes: &init,
        perms: MemFlags::user_rx(),
    }];
    let image = UserImage {
        segments: &segments,
        entry: VirtualAddress::new(USER_SEGMENT_VA),
        user_stack_top: VirtualAddress::new(USER_STACK_TOP_VA),
        user_stack_size: USER_STACK_SIZE,
    };

    let err = scheduler
        .spawn_user_process_with_launch(
            "bad-launch",
            &image,
            Priority::new(2),
            4,
            UserProcessLaunch::new().bootstrap_handle(0),
        )
        .unwrap_err();

    assert_eq!(
        err,
        SpawnUserError::Prepared(scheduler::PreparedUserProcessError::InvalidBootstrapHandle)
    );
    assert_eq!(factory.created(), 0);
    assert_eq!(scheduler.process_count(), 1);
}

#[test]
fn spawn_user_process_loads_segments_and_stack_in_order() {
    reset_switches();
    let factory = fresh_factory();
    let timer = MockTimer::new();
    let scheduler = make_scheduler(timer, factory);

    let init0 = [0x11u8; 16];
    let init1 = [0x22u8; 32];
    let segments = [
        UserSegment {
            va_base: aligned(USER_SEGMENT_VA),
            mapped_size: PAGE,
            init_bytes: &init0,
            perms: MemFlags::user_rx(),
        },
        UserSegment {
            va_base: aligned(USER_SEGMENT_VA + PAGE),
            mapped_size: PAGE,
            init_bytes: &init1,
            perms: MemFlags::user_rw(),
        },
    ];
    let image = UserImage {
        segments: &segments,
        entry: VirtualAddress::new(USER_SEGMENT_VA),
        user_stack_top: VirtualAddress::new(USER_STACK_TOP_VA),
        user_stack_size: PAGE, // 1 страница стека для проверки порядка
    };

    scheduler
        .spawn_user_process("user-b", &image, Priority::new(2), 4)
        .expect("spawn ok");

    let calls = factory.map_calls();
    assert_eq!(calls.len(), 3, "2 segments + stack");
    assert_eq!(calls[0].va, USER_SEGMENT_VA);
    assert_eq!(calls[1].va, USER_SEGMENT_VA + PAGE);
    let stack_base = USER_STACK_TOP_VA - PAGE;
    assert_eq!(calls[2].va, stack_base);

    // map() получает init-байты segment-а как есть (хвост зануляет уже
    // сам mapper) - проверяем хеш именно от слайса init.
    assert_eq!(calls[0].init_hash, common::fnv1a_hash(&init0));
    assert_eq!(calls[0].init_len, init0.len());
    assert_eq!(calls[1].init_hash, common::fnv1a_hash(&init1));
}

#[test]
fn spawn_user_process_validation_rejects_overlapping_segments() {
    reset_switches();
    let factory = fresh_factory();
    let timer = MockTimer::new();
    let scheduler = make_scheduler(timer, factory);

    let segments = [
        UserSegment {
            va_base: aligned(USER_SEGMENT_VA),
            mapped_size: 2 * PAGE,
            init_bytes: &[],
            perms: MemFlags::user_rx(),
        },
        UserSegment {
            va_base: aligned(USER_SEGMENT_VA + PAGE),
            mapped_size: PAGE,
            init_bytes: &[],
            perms: MemFlags::user_rw(),
        },
    ];
    let image = UserImage {
        segments: &segments,
        entry: VirtualAddress::new(USER_SEGMENT_VA),
        user_stack_top: VirtualAddress::new(USER_STACK_TOP_VA),
        user_stack_size: USER_STACK_SIZE,
    };

    let err = scheduler
        .spawn_user_process("bad", &image, Priority::new(2), 4)
        .unwrap_err();
    assert_eq!(
        err,
        SpawnUserError::Image(UserImageError::OverlappingSegments)
    );
    // validate отрабатывает до `factory.create_user()` - фабрика не была
    // потревожена, root-фрейм user-AS не аллоцировался.
    assert_eq!(factory.created(), 0);
    assert_eq!(factory.released(), 0);
    // Только idle процесс - user не вставился.
    assert_eq!(scheduler.process_count(), 1);
}

#[test]
fn spawn_user_process_validation_rejects_misaligned() {
    reset_switches();
    let factory = fresh_factory();
    let timer = MockTimer::new();
    let scheduler = make_scheduler(timer, factory);

    let segments = [UserSegment {
        va_base: aligned(USER_SEGMENT_VA),
        mapped_size: PAGE + 1, // не кратно 4К
        init_bytes: &[],
        perms: MemFlags::user_rx(),
    }];
    let image = UserImage {
        segments: &segments,
        entry: VirtualAddress::new(USER_SEGMENT_VA),
        user_stack_top: VirtualAddress::new(USER_STACK_TOP_VA),
        user_stack_size: USER_STACK_SIZE,
    };

    let err = scheduler
        .spawn_user_process("bad", &image, Priority::new(2), 4)
        .unwrap_err();
    assert_eq!(
        err,
        SpawnUserError::Image(UserImageError::MisalignedSegment)
    );
    // Только idle процесс - user не вставился.
    assert_eq!(scheduler.process_count(), 1);
    // validate отрабатывает раньше первого `mapper.map()` - лог пуст.
    assert!(factory.map_calls().is_empty());
}

#[test]
fn spawn_user_process_without_factory_returns_missing_factory() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler =
        TestScheduler::with_address_space_factory(MockTimerSource(timer), TEST_CONFIG, None)
            .bootstrap();

    let init = [0xAAu8; 4];
    let segments = [UserSegment {
        va_base: aligned(USER_SEGMENT_VA),
        mapped_size: PAGE,
        init_bytes: &init,
        perms: MemFlags::user_rx(),
    }];
    let image = UserImage {
        segments: &segments,
        entry: VirtualAddress::new(USER_SEGMENT_VA),
        user_stack_top: VirtualAddress::new(USER_STACK_TOP_VA),
        user_stack_size: USER_STACK_SIZE,
    };

    let err = scheduler
        .spawn_user_process("u", &image, Priority::new(2), 4)
        .unwrap_err();
    assert_eq!(err, SpawnUserError::MissingFactory);
}

#[test]
fn context_switch_kernel_to_user_records_user_root() {
    reset_switches();
    let factory = fresh_factory();
    let timer = MockTimer::new();
    let scheduler = make_scheduler(timer.clone(), factory);

    let init = [0xAAu8; 4];
    let segments = [UserSegment {
        va_base: aligned(USER_SEGMENT_VA),
        mapped_size: PAGE,
        init_bytes: &init,
        perms: MemFlags::user_rx(),
    }];
    let image = UserImage {
        segments: &segments,
        entry: VirtualAddress::new(USER_SEGMENT_VA),
        user_stack_top: VirtualAddress::new(USER_STACK_TOP_VA),
        user_stack_size: USER_STACK_SIZE,
    };

    scheduler
        .spawn_user_process("u", &image, Priority::new(2), 4)
        .unwrap();

    let _running = scheduler.run();
    let switches = take_address_space_switches();
    // Root первой выданной фабрикой пары.
    let expected_root = MockAddressSpaceFactory::BASE_ROOT_PA;
    assert!(
        switches
            .iter()
            .any(|sw| matches!(sw, Some(handle) if handle.root.as_usize() == expected_root)),
        "kernel->user switch must include Some(user_root); got {switches:?}"
    );
}

#[test]
fn context_switch_between_two_user_processes_writes_distinct_roots() {
    reset_switches();
    let factory = fresh_factory();
    let timer = MockTimer::new();
    let scheduler = make_scheduler(timer, factory);

    let init = [0xAAu8; 4];
    let segs_a = [UserSegment {
        va_base: aligned(USER_SEGMENT_VA),
        mapped_size: PAGE,
        init_bytes: &init,
        perms: MemFlags::user_rx(),
    }];
    let image_a = UserImage {
        segments: &segs_a,
        entry: VirtualAddress::new(USER_SEGMENT_VA),
        user_stack_top: VirtualAddress::new(USER_STACK_TOP_VA),
        user_stack_size: USER_STACK_SIZE,
    };
    let segs_b = [UserSegment {
        va_base: aligned(USER_SEGMENT_VA),
        mapped_size: PAGE,
        init_bytes: &init,
        perms: MemFlags::user_rx(),
    }];
    let image_b = UserImage {
        segments: &segs_b,
        entry: VirtualAddress::new(USER_SEGMENT_VA),
        user_stack_top: VirtualAddress::new(USER_STACK_TOP_VA),
        user_stack_size: USER_STACK_SIZE,
    };

    scheduler
        .spawn_user_process("user-a", &image_a, Priority::new(2), 4)
        .unwrap();
    scheduler
        .spawn_user_process("user-b", &image_b, Priority::new(2), 4)
        .unwrap();

    let running = scheduler.run();
    take_address_space_switches();

    running.yield_now();
    let after_yield = take_address_space_switches();

    let root_a = MockAddressSpaceFactory::BASE_ROOT_PA;
    let root_b = root_a + 4096;
    assert!(
        after_yield
            .iter()
            .any(|sw| matches!(sw, Some(handle) if handle.root.as_usize() == root_b)),
        "yield user-a->user-b must switch to root_b={root_b:#x}, got {after_yield:?}"
    );
    let _ = root_a;
}

#[test]
fn context_switch_between_threads_of_same_user_process_does_not_change_address_space() {
    reset_switches();
    let factory = fresh_factory();
    let timer = MockTimer::new();
    let scheduler = make_scheduler(timer, factory);

    let init = [0xAAu8; 4];
    let segments = [UserSegment {
        va_base: aligned(USER_SEGMENT_VA),
        mapped_size: PAGE,
        init_bytes: &init,
        perms: MemFlags::user_rx(),
    }];
    let image = UserImage {
        segments: &segments,
        entry: VirtualAddress::new(USER_SEGMENT_VA),
        user_stack_top: VirtualAddress::new(USER_STACK_TOP_VA),
        user_stack_size: USER_STACK_SIZE,
    };

    scheduler
        .spawn_user_process("user", &image, Priority::new(2), 4)
        .unwrap();

    // Запускаем scheduler - попадаем в первый user-thread.
    let running = scheduler.run();

    // Через handle добавляем второй thread в тот же процесс - Inherit
    // снимает AS из current (user) и инкрементирует thread_count.
    let handle = std::sync::Arc::new(running.handle());
    let service: std::sync::Arc<dyn SchedulerService> = handle;
    service
        .spawn(
            SpawnConfig::new("u-second-thread")
                .address_space(SpawnAddressSpace::Inherit)
                .priority(Priority::new(2)),
            || {},
        )
        .expect("spawn second thread inheriting AS");

    // Сбрасываем лог переключений ДО yield_now.
    take_address_space_switches();
    running.yield_now();
    let after_yield = take_address_space_switches();

    // Между двумя потоками одного процесса AS не должен переключаться
    // (Keep-ветка `address_space_change`).
    assert!(
        after_yield.is_empty(),
        "switch within same user process must not call switch_address_space; got {after_yield:?}"
    );
}

#[test]
fn last_thread_exit_releases_address_space() {
    reset_switches();
    let factory = fresh_factory();
    let timer = MockTimer::new();
    let scheduler = make_scheduler(timer, factory);

    let init = [0xAAu8; 4];
    let segments = [UserSegment {
        va_base: aligned(USER_SEGMENT_VA),
        mapped_size: PAGE,
        init_bytes: &init,
        perms: MemFlags::user_rx(),
    }];
    let image = UserImage {
        segments: &segments,
        entry: VirtualAddress::new(USER_SEGMENT_VA),
        user_stack_top: VirtualAddress::new(USER_STACK_TOP_VA),
        user_stack_size: USER_STACK_SIZE,
    };

    scheduler
        .spawn_user_process("u", &image, Priority::new(2), 4)
        .unwrap();
    assert_eq!(factory.released(), 0);
    // process_count: idle + user = 2.
    assert_eq!(scheduler.process_count(), 2);

    let running = scheduler.run();
    // dying user-thread -> exit_current -> switch на idle.
    running.exit_current();

    // Oracle: cleanup_pending removed the user Process and released its mapper.
    assert_eq!(factory.released(), 1, "user-AS must be released after exit");
    // Только idle остался.
    assert_eq!(running.process_count(), 1);
}

#[test]
fn spawn_user_process_initializes_user_vm_allocator_between_image_and_stack() {
    reset_switches();
    let factory = fresh_factory();
    let timer = MockTimer::new();
    let scheduler = make_scheduler(timer, factory);

    let init = [0xAAu8; 8];
    let segments = [UserSegment {
        va_base: aligned(USER_SEGMENT_VA),
        mapped_size: PAGE,
        init_bytes: &init,
        perms: MemFlags::user_rx(),
    }];
    let image = UserImage {
        segments: &segments,
        entry: VirtualAddress::new(USER_SEGMENT_VA),
        user_stack_top: VirtualAddress::new(USER_STACK_TOP_VA),
        user_stack_size: USER_STACK_SIZE,
    };

    let (pid, _tid) = scheduler
        .spawn_user_process("vm-bench", &image, Priority::new(2), 4)
        .expect("spawn user process");

    // У свежесозданного процесса аллокатор пуст: ни одного региона ещё не
    // выделено через vm_allocate.
    assert_eq!(scheduler.process_user_vm_region_count(pid), Some(0));
}

#[test]
fn spawning_two_user_processes_creates_two_distinct_address_spaces() {
    reset_switches();
    let factory = fresh_factory();
    let timer = MockTimer::new();
    let scheduler = make_scheduler(timer, factory);

    let init = [0xAAu8; 4];
    let segments = [UserSegment {
        va_base: aligned(USER_SEGMENT_VA),
        mapped_size: PAGE,
        init_bytes: &init,
        perms: MemFlags::user_rx(),
    }];
    let image = UserImage {
        segments: &segments,
        entry: VirtualAddress::new(USER_SEGMENT_VA),
        user_stack_top: VirtualAddress::new(USER_STACK_TOP_VA),
        user_stack_size: USER_STACK_SIZE,
    };

    scheduler
        .spawn_user_process("a", &image, Priority::new(2), 4)
        .unwrap();
    scheduler
        .spawn_user_process("b", &image, Priority::new(2), 4)
        .unwrap();

    assert_eq!(factory.created(), 2);
    // process_count: idle + 2 user = 3.
    assert_eq!(scheduler.process_count(), 3);
    // Каждый процесс - segment + stack = 2 вызова map(); итого 4.
    assert_eq!(factory.map_calls().len(), 4);
}
