//! Стенд стандартной библиотеки: программу гоняет `testrunner` управляемым компонентом.
//!
//! Проверки идут обычными `assert!`. Провал роняет программу паникой, её сообщение уходит в
//! журнал через `stderr`, а вызывающий видит ненулевой код выхода. Успех — код ноль.
//!
//! Проверяется поверхность самой `std` поверх платформенного слоя. Примитивы `molecular-rt`
//! под ней покрыты набором `testrunner` отдельно.

#![feature(molecular_ext)]

use std::{
    cell::Cell,
    os::molecular,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

mod local {
    // Линт срабатывает на уже константном инициализаторе изнутри раскрытия макроса, куда
    // атрибут с места вызова не доходит.
    #![allow(clippy::missing_const_for_thread_local)]

    use std::cell::Cell;

    thread_local! {
        pub static LOCAL: Cell<u32> = const { Cell::new(0) };
    }
}

fn main() {
    startup_set_reaches_the_program();
    heap_serves_growing_collections();
    monotonic_clock_moves_forward();
    mutex_hands_out_exclusive_access();
    threads_share_state_under_a_mutex();
    a_thread_hands_back_its_value();
    thread_locals_stay_private_to_their_thread();
    sleep_waits_out_its_duration();
    an_unrepresentable_stack_size_is_refused();
    spawning_outlives_the_handle_table();
    println!("checks passed");
}

fn startup_set_reaches_the_program() {
    let startup = molecular::startup().expect("composition hands out the startup set");
    assert_eq!(startup.slots().count(), 0, "the manifest declares no slots");
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
