//! Обработчик паники ядра: печать причины и выключение машины.

use core::{
    hint::spin_loop,
    panic::PanicInfo,
    sync::atomic::{AtomicU64, Ordering},
};

use kbase::{CriticalSection as _, DaifCriticalSection};

use crate::{cpu, smp::Counter};

const NESTED_PANIC_EXIT_CODE: i32 = 3;

const REPORTER_TIMEOUT_MS: u64 = 2_000;

const NO_REPORTER: u64 = u64::MAX;

static REPORTER_AFFINITY: AtomicU64 = AtomicU64::new(NO_REPORTER);

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // IRQ маскируются на весь путь паники, поэтому печать причины не прерывается,
    // а обработчик не вытесняется до выключения машины.
    let _irq_mask = DaifCriticalSection::enter();

    let this_cpu = cpu::current_affinity().raw();
    match REPORTER_AFFINITY.compare_exchange(
        NO_REPORTER,
        this_cpu,
        Ordering::Relaxed,
        Ordering::Relaxed,
    ) {
        Ok(_) => {}
        // Паника на пути печати причины гасит машину без печати, потому что стек ядра
        // защитной страницы не имеет, и рекурсивная печать затирает соседнюю память.
        Err(reporter) if reporter == this_cpu => {
            kernelspace::power::system_off(NESTED_PANIC_EXIT_CODE)
        }
        // Причину печатает CPU, первым вошедший в панику. Остальные дают ему время выключить
        // машину и сами выключают её, если он завис.
        Err(_) => wait_for_reporter(),
    }

    // Причина печатается в обход локов пути записи, потому что их держатель - контекст,
    // прерванный паникой, и своих локов он уже не освобождает.
    klog::panic_logf(format_args!("Kernel panic: {info}"));
    kernelspace::power::system_off(1)
}

fn wait_for_reporter() -> ! {
    if let Some(counter) = Counter::probe() {
        let deadline = counter.deadline_after_ms(REPORTER_TIMEOUT_MS);
        while !Counter::passed(deadline) {
            spin_loop();
        }
    }

    kernelspace::power::system_off(1)
}
