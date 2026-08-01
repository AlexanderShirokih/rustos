//! Жизненный цикл регистрации стока паники.
//!
//! Файл держит единственный тест: сценарий зависит от нетронутых статиков
//! `klog`, которые свежи только в собственном тестовом бинаре.

mod common;

use std::sync::Arc;

use common::Recorder;

#[test]
fn first_registered_panic_sink_wins() {
    // Вызов до регистрации стока обязан вернуться, а не зависнуть.
    klog::panic_printf(format_args!("dropped-marker"));

    let first = Arc::new(Recorder::default());
    klog::set_panic_stdout(first.clone());
    klog::panic_printf(format_args!("first-marker"));

    let second = Arc::new(Recorder::default());
    klog::set_panic_stdout(second.clone());
    klog::panic_printf(format_args!("second-marker"));

    assert!(first.panic_contents().contains("first-marker"));
    assert!(first.panic_contents().contains("second-marker"));
    assert!(second.panic_contents().is_empty());
}
