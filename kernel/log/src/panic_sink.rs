use alloc::sync::Arc;
use core::fmt::{Arguments, Write as _};

use io::writer::Writer;
use spin::Once;

use crate::level::{Level, prefix};

type PanicWriter = dyn Writer + Send + Sync;

static PANIC_STDOUT: Once<Arc<PanicWriter>> = Once::new();

struct PanicFmtWriter<'a>(&'a PanicWriter);

impl core::fmt::Write for PanicFmtWriter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.0.write_all_panic(s.as_bytes());
        Ok(())
    }
}

/// Регистрирует сток пути паники: writer, чей путь записи свободен от локов.
/// Первый вызов фиксирует сток, повторные игнорируются.
pub fn set_panic_stdout(w: Arc<PanicWriter>) {
    PANIC_STDOUT.call_once(|| w);
}

/// Печатает причину паники панической стороной стока ([`Writer::write_all_panic`]),
/// минуя лок `STDOUT`. До регистрации стока вывод отбрасывается.
pub fn panic_printf(arguments: Arguments) {
    // Сток хранится отдельно от `STDOUT` и читается неблокирующим `Once::get`:
    // лок пути записи на пути паники уже никем не освобождается.
    let Some(writer) = PANIC_STDOUT.get() else {
        return;
    };

    let mut fmt = PanicFmtWriter(&**writer);
    let _ = fmt.write_fmt(arguments);
    writer.flush();
}

/// Лог причины паники: [`panic_printf`] с префиксом уровня и переводом строки.
pub fn panic_logf(arguments: Arguments) {
    let level = prefix(Level::Fatal);

    panic_printf(format_args!("{level}{arguments}\r\n"));
}
