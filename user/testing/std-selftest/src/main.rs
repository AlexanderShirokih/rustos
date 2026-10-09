//! Стенд стандартной библиотеки. Программу запускает тестовый родитель `testrunner`.
//!
//! Проверки идут обычными `assert!`. Провал завершает программу паникой, её сообщение уходит
//! в журнал через `stderr`, а вызывающий получает ненулевой код выхода. Успех - код ноль.
//!
//! Проверяется поверхность самой `std` поверх платформенного слоя. Его примитивы
//! под ней покрыты набором `testrunner` отдельно.

#![feature(
    can_vector,
    fs_set_times,
    io_error_too_many_open_files,
    molecular_ext,
    set_permissions_nofollow
)]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::std_instead_of_core
)]

use std::{
    cell::Cell,
    io::ErrorKind,
    os::molecular,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use components::{StartupError, component_manifest, into_std_handle};
use filesystem_abi::namespace;
use kernel_objects::{Port, Signal};
use rtc_abi::rtc;
use syscall::{SIGNALED, SyscallError, WakeCount};

mod filesystem;

component_manifest! {
    struct SelftestStartup {
        requirements: {
            namespace: session(namespace::Protocol),
            rtc: session(rtc::Protocol),
        },
    }
}

mod local {
    // Линт срабатывает на уже константном инициализаторе изнутри раскрытия макроса, куда
    // атрибут с места вызова не доходит.
    #![allow(clippy::missing_const_for_thread_local)]

    use std::cell::Cell;

    thread_local! {
        pub static LOCAL: Cell<u32> = const { Cell::new(0) };
    }
}

fn main() -> Result<(), StartupError> {
    println!("selftest started");
    let requirements = components::requirements::<SelftestStartup>()?;
    let namespace = requirements.namespace.open()?;
    let rtc = requirements.rtc.open()?;
    the_start_handle_is_handed_out_once();
    filesystem::a_path_before_the_root_is_not_connected();
    filesystem::the_root_is_installed(namespace.into_handle());
    filesystem::a_second_root_is_handed_back();
    an_owned_handle_closes_its_capability();
    heap_serves_growing_collections();
    monotonic_clock_moves_forward();
    the_realtime_clock_is_installed(rtc.into_handle());
    a_second_realtime_clock_is_refused();
    system_time_follows_the_monotonic_clock();
    mutex_hands_out_exclusive_access();
    threads_share_state_under_a_mutex();
    a_thread_hands_back_its_value();
    thread_locals_stay_private_to_their_thread();
    sleep_waits_out_its_duration();
    an_unrepresentable_stack_size_is_refused();
    spawning_outlives_the_handle_table();
    filesystem::open_options_follow_their_flags();
    filesystem::reads_writes_and_seeks_share_one_position();
    filesystem::an_append_lands_at_the_end_in_one_piece();
    filesystem::a_clone_shares_the_position();
    filesystem::two_files_in_two_threads_stay_apart();
    filesystem::metadata_holds_only_the_kind_and_the_size();
    filesystem::the_image_reads_and_refuses_writes();
    filesystem::unsupported_file_operations_say_so();
    filesystem::malformed_paths_are_refused();
    filesystem::file_system_errors_keep_their_meaning();
    filesystem::a_listing_outlives_other_calls();
    filesystem::a_dropped_listing_closes_its_session();
    filesystem::create_dir_refuses_an_existing_directory();
    filesystem::rename_stays_within_one_mount();
    filesystem::removal_checks_the_node_kind();
    filesystem::remove_dir_all_takes_the_subtree();
    filesystem::copy_moves_the_bytes();
    filesystem::exists_hides_only_not_found();
    filesystem::canonicalize_normalizes_lexically();
    filesystem::unsupported_path_operations_say_so();
    println!("checks passed");
    Ok(())
}

fn the_start_handle_is_handed_out_once() {
    assert!(
        molecular::take_start_handle().is_none(),
        "the start handle is handed out twice"
    );
}

fn an_owned_handle_closes_its_capability() {
    let signal = Signal::create().expect("the signal is created");
    let raw = signal.as_raw();
    drop(into_std_handle(signal));

    assert_eq!(
        syscall_user::signal_set(raw, SIGNALED, 0, WakeCount::None),
        SyscallError::BadHandle.as_return_value(),
        "the dropped handle stays open"
    );
}

fn heap_serves_growing_collections() {
    let mut values: Vec<usize> = Vec::new();
    for value in 0..4096 {
        values.push(value);
    }
    assert_eq!(values.iter().sum::<usize>(), 4096 * 4095 / 2);

    let text = values[..4].iter().map(usize::to_string).collect::<Vec<_>>();
    assert_eq!(text.join(","), "0,1,2,3");
}

fn monotonic_clock_moves_forward() {
    let started = Instant::now();
    while started.elapsed() == Duration::ZERO {
        core::hint::spin_loop();
    }
    let elapsed = started.elapsed();
    assert!(elapsed > Duration::ZERO, "clock stands still");
    assert!(Instant::now() >= started, "clock runs backwards");
    assert_eq!(
        (started + elapsed).duration_since(started),
        elapsed,
        "duration arithmetic disagrees with the clock"
    );
    println!("monotonic clock advanced by {elapsed:?}");
}

fn the_realtime_clock_is_installed(rtc: Port) {
    molecular::install_realtime_clock(into_std_handle(rtc))
        .expect("the first realtime clock is installed");
}

fn a_second_realtime_clock_is_refused() {
    let spare = into_std_handle(Port::create().expect("the port is created"));

    assert_eq!(
        molecular::install_realtime_clock(spare).map_err(|error| error.kind()),
        Err(ErrorKind::AlreadyExists),
        "a second realtime clock replaces the first"
    );
}

fn system_time_follows_the_monotonic_clock() {
    const NAP: Duration = Duration::from_millis(20);

    let before = SystemTime::now();
    thread::sleep(NAP);
    let after = SystemTime::now();
    assert!(
        after
            .duration_since(before)
            .is_ok_and(|passed| passed >= NAP),
        "system time advanced by less than the nap"
    );

    let since_epoch = after
        .duration_since(UNIX_EPOCH)
        .expect("system time follows the epoch");
    println!("time {}", since_epoch.as_secs());
}

fn mutex_hands_out_exclusive_access() {
    let counter = Mutex::new(0u32);
    for _ in 0..16 {
        *counter.lock().expect("mutex is not poisoned") += 1;
    }
    assert_eq!(*counter.lock().expect("mutex is not poisoned"), 16);
}

fn threads_share_state_under_a_mutex() {
    const WORKERS: u32 = 4;
    const STEPS: u32 = 1000;

    let counter = Arc::new(Mutex::new(0u32));
    let workers: Vec<_> = (0..WORKERS)
        .map(|_| {
            let counter = Arc::clone(&counter);
            thread::spawn(move || {
                for _ in 0..STEPS {
                    *counter.lock().expect("mutex is not poisoned") += 1;
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().expect("worker runs to its end");
    }

    assert_eq!(
        *counter.lock().expect("mutex is not poisoned"),
        WORKERS * STEPS
    );
}

fn a_thread_hands_back_its_value() {
    let value = thread::spawn(|| "handed back".to_string())
        .join()
        .expect("worker runs to its end");
    assert_eq!(value, "handed back");
}

fn thread_locals_stay_private_to_their_thread() {
    local::LOCAL.with(|local| local.set(17));
    let seen = thread::spawn(|| {
        let before = local::LOCAL.with(Cell::get);
        local::LOCAL.with(|local| local.set(42));
        (before, local::LOCAL.with(Cell::get))
    })
    .join()
    .expect("worker runs to its end");

    assert_eq!(seen, (0, 42), "the worker sees its own local");
    assert_eq!(
        local::LOCAL.with(Cell::get),
        17,
        "the worker did not touch ours"
    );
}

fn sleep_waits_out_its_duration() {
    const NAP: Duration = Duration::from_millis(20);

    let started = Instant::now();
    thread::sleep(NAP);
    assert!(started.elapsed() >= NAP, "sleep returned early");
}

fn an_unrepresentable_stack_size_is_refused() {
    let refused = thread::Builder::new().stack_size(usize::MAX).spawn(|| ());
    assert!(
        refused.is_err(),
        "a stack size past the address space started a thread"
    );
}

fn spawning_outlives_the_handle_table() {
    // Запись таблицы handle, занятая запуском потока, возвращается к следующему кругу.
    let rounds = syscall::HANDLE_TABLE_CAPACITY + 16;
    for round in 0..rounds {
        thread::spawn(|| ())
            .join()
            .unwrap_or_else(|_| panic!("worker {round} of {rounds} runs to its end"));
    }
}
