//! Дескриптор тест-кейса и низкоуровневая регистрация в линкер-секции.

#![allow(unsafe_code)]

#[repr(C)]
pub struct TestCase {
    /// Выводимое имя теста.
    pub name: &'static str,
    /// Путь теста `<module>::<function>`.
    pub path: &'static str,
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

/// Регистрирует тест-кейс в секции `.tests`.
///
/// Раскрытие кладёт в эту секцию статик `$symbol` типа `TestCase`.
/// [`crate::runner::run_tests`] обходит секцию между линкер-символами `__tests_start` и
/// `__tests_end` и берёт из неё записи выбранного `kind`.
///
/// Обычно вызывается не напрямую, а через атрибут [`test`](crate::test).
///
/// ```ignore
/// #[test_harness::test]
/// fn meta_test() {}
/// ```
#[macro_export]
macro_rules! register_test {
    ($symbol:ident, $name:literal, $run:expr, $kind:expr) => {
        // Секцию раскладывает линкер-скрипт: у ядра свой, у testrunner свой. Оба таргета
        // перечислены здесь, потому что проходы `qemu-test` идут и там, и там.
        #[cfg_attr(
            any(target_os = "none", target_os = "molecular"),
            unsafe(link_section = ".tests")
        )]
        #[used]
        static $symbol: $crate::case::TestCase = $crate::case::TestCase {
            name: $name,
            path: ::core::concat!(::core::module_path!(), "::", $name),
            run: $run,
            kind: $kind,
        };
    };
}
