//! Дескриптор тест-кейса и макрос регистрации в линкер-секции.

#![allow(unsafe_code)]

#[repr(C)]
pub struct TestCase {
    pub name: &'static str,
    pub run: fn(),
}

/// Регистрирует тест-кейс в секции `.tests.kernel`.
///
/// ```ignore
/// fn meta_test() { /* ... */ }
/// qemu_test_harness::register_test!(META_TEST, "meta", meta_test);
/// ```
#[macro_export]
macro_rules! register_test {
    ($symbol:ident, $name:expr, $run:expr) => {
        #[cfg_attr(target_os = "none", unsafe(link_section = ".tests.kernel"))]
        #[used]
        static $symbol: $crate::case::TestCase = $crate::case::TestCase {
            name: $name,
            run: $run,
        };
    };
}
