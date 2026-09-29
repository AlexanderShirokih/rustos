//! Точка входа фикстуры, для которой прогон проверяет только исход "прошла или нет".
//!
//! `fixture_start!(run)` объявляет кучу процесса, `_start` и `panic_handler`. `_start` передаёт
//! владельца стартового handle в `run` и завершает поток кодом 0, если `run` вернула `Ok`, и
//! кодом 1 иначе. Паника и запуск без стартового handle тоже завершают поток кодом 1.

/// Отказ шага фикстуры.
pub struct Failed;

macro_rules! fixture_start {
    ($run:path) => {
        runtime::process_heap!();

        #[unsafe(no_mangle)]
        pub extern "C" fn _start(start: u32) -> ! {
            // SAFETY: `start` - значение `x0` точки входа, и точка входа исполняется один раз.
            unsafe { runtime::start(start) };
            let Some(start) = runtime::take_bootstrap() else {
                runtime::thread_exit(1)
            };
            runtime::thread_exit(u64::from($run(start.assume_kind()).is_err()))
        }

        #[panic_handler]
        fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
            runtime::thread_exit(1)
        }
    };
}

pub(crate) use fixture_start;
