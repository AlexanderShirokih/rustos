//! Дескриптор тест-кейса и низкоуровневая регистрация в линкер-секции.

#![allow(unsafe_code)]

#[repr(C)]
pub struct TestCase {
    pub name: &'static str,
    pub run: fn(),
    pub kind: TestKind,
}

/// Профиль зарегистрированного тест-кейса.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestKind {
    /// Короткая проверка, входящая в обычный `qemu-test`.
    Regular,
    /// Длительный воспроизводимый прогон с seed, доступный только проходу testrunner.
    Stress,
    /// Замер, печатающий показания вместо проверки условия.
    Bench,
}

/// Регистрирует тест-кейс в секции `.tests.kernel`.
///
/// Раскрытие кладёт в эту секцию статик `$symbol: TestCase { name: $name, run: $run,
/// kind: $kind }`. [`crate::runner::run_tests`] обходит секцию между линкер-символами
/// `__tests_kernel_start` и `__tests_kernel_end` и берёт из неё записи выбранного `kind`.
///
/// Обычно вызывается не напрямую, а через атрибут `kernel_test`.
///
/// ```ignore
/// use kernel_tests::kernel_test;
///
/// #[kernel_test]
/// fn meta_test() {}
/// ```
#[macro_export]
macro_rules! register_test {
    ($symbol:ident, $name:expr, $run:expr, $kind:expr) => {
        // Секцию раскладывает линкер-скрипт: у ядра свой, у testrunner свой. Оба таргета
        // перечислены здесь, потому что проходы `qemu-test` идут и там, и там.
        #[cfg_attr(
            any(target_os = "none", target_os = "molecular"),
            unsafe(link_section = ".tests.kernel")
        )]
        #[used]
        static $symbol: $crate::case::TestCase = $crate::case::TestCase {
            name: $name,
            run: $run,
            kind: $kind,
        };
    };
}
