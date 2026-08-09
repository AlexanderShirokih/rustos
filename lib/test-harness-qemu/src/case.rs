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
}

/// Регистрирует тест-кейс в секции `.tests.kernel`.
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
        #[cfg_attr(target_os = "none", unsafe(link_section = ".tests.kernel"))]
        #[used]
        static $symbol: $crate::case::TestCase = $crate::case::TestCase {
            name: $name,
            run: $run,
            kind: $kind,
        };
    };
}
