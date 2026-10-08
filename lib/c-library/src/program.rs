//! Связь программы с рантаймом C: имя программы для `argv[0]`.

/// Имя программы со стороны C: строка UTF-8 с завершающим нулём.
pub struct ProgramName(&'static str);

impl ProgramName {
    /// Принимает имя с завершающим нулём. Строку без него отвергает компиляция статика.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        assert!(
            matches!(name.as_bytes().last(), Some(0)),
            "the program name must end with a NUL byte"
        );
        Self(name)
    }

    pub(crate) const fn as_bytes(&self) -> &'static [u8] {
        self.0.as_bytes()
    }
}

/// Объявляет C-программу. Макрос связывает пакет с рантаймом C и задаёт имя bin-таргета как
/// `argv[0]` её функции `main`.
///
/// Ставится в корне no_std-пакета без своей точки входа, обработчика паники и аллокатора.
#[macro_export]
macro_rules! c_program {
    () => {
        #[unsafe(no_mangle)]
        static C_LIBRARY_PROGRAM_NAME: $crate::ProgramName =
            $crate::ProgramName::new(concat!(env!("CARGO_BIN_NAME"), "\0"));
    };
}
