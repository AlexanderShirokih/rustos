//! Точка входа фикстуры, для которой прогон проверяет только исход "прошла или нет".
//!
//! `fixture_start!(run)` объявляет кучу процесса, `_start` и `panic_handler`. `_start` передаёт
//! стартовый handle в `run` и завершает поток кодом 0, если `run` вернула `Ok`, и кодом 1
//! иначе. Паника тоже завершает поток кодом 1.

/// Отказ шага фикстуры.
pub struct Failed;

macro_rules! fixture_start {
    ($run:path) => {
        runtime::process_heap!();

        #[unsafe(no_mangle)]
        pub extern "C" fn _start(start: syscall::Handle<syscall::kind::Port>) -> ! {
            runtime::thread_exit(u64::from($run(start).is_err()))
        }

        #[panic_handler]
        fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
            runtime::thread_exit(1)
        }
    };
}

pub(crate) use fixture_start;
