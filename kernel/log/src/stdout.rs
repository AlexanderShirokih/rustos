use core::fmt::{Arguments, Write as _};

use collections::{LockCell, MutexCell};
use io::writer::Writer;

use crate::level::{Level, prefix};

type StaticWriter = dyn Writer + Sync + 'static;

struct NilWriter;
impl Writer for NilWriter {
    fn write_all(&self, _: &[u8]) {}
    fn flush(&self) {}
}

static NIL: NilWriter = NilWriter;

static STDOUT: MutexCell<&'static StaticWriter> = MutexCell::new(&NIL);

struct FmtWriter<'a>(&'a StaticWriter);

impl core::fmt::Write for FmtWriter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.0.write_all(s.as_bytes());
        Ok(())
    }
}

/// Устанавливает writer для вывода логов.
/// Можно вызывать повторно, чтобы заменить writer.
pub fn set_stdout(w: &'static StaticWriter) {
    STDOUT.with_lock(|writer| *writer = w);
}

/// Печать форматированной строки без префикса уровня.
pub fn printf(arguments: Arguments) {
    STDOUT.with_lock(|writer| {
        let mut fmt = FmtWriter(*writer);
        let _ = fmt.write_fmt(arguments);
        writer.flush();
    });
}

/// Лог с префиксом уровня и переводом строки.
pub fn logf(lvl: Level, arguments: Arguments) {
    let level = prefix(lvl);

    printf(format_args!("{level}{arguments}\r\n"));
}

/// Лог с префиксом уровня, тегом и переводом строки.
pub fn logf_tagged(lvl: Level, tag: &str, arguments: Arguments) {
    let level = prefix(lvl);

    printf(format_args!("{level}[{tag}] {arguments}\r\n"));
}

/// Возвращает имя файла без пути для использования в качестве тега.
pub fn file_tag(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}
