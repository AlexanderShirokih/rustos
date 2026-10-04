//! Точка входа фикстуры, код выхода которой не содержит данных.
//!
//! `fixture_start!(run)` объявляет кучу процесса, `_start` и `panic_handler`. `_start` держит
//! владельца стартового handle до завершения процесса и передаёт `run` ссылку на него. Если `run`
//! вернула `Ok`, процесс завершается кодом 0, иначе кодом 1. Паника в любом потоке и запуск без
//! стартового handle тоже завершают процесс кодом 1. Когда фикстуру запускает rootkeeper, код 1
//! означает отказ записи плана, и rootkeeper перезапускает запись, пока у неё есть перезапуски.

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
                runtime::process_exit(1)
            };
            let start = kernel_objects::OwnedHandle::from(start.assume_kind());
            runtime::process_exit(u64::from($run(&start).is_err()))
        }

        #[panic_handler]
        fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
            runtime::process_exit(1)
        }
    };
}

pub(crate) use fixture_start;
