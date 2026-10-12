use alloc::{boxed::Box, string::String, vec::Vec};
use core::{
    fmt::{Arguments, Write as _},
    sync::atomic::AtomicU64,
};

use io::writer::Writer;
use kbase::{LockCell, MutexCell};
use log_abi::{LOG_MESSAGE_MAX, LogBuffer, Record, RingWriter};

use crate::level::{Level, prefix};

type StaticWriter = dyn Writer + Sync + 'static;

/// Источник времени строк лога в наносекундах.
pub type Clock = Box<dyn Fn() -> u64 + Send>;

const NS_PER_SECOND: u64 = 1_000_000_000;
const NS_PER_MILLISECOND: u64 = 1_000_000;

struct NilWriter;
impl Writer for NilWriter {
    fn write_all(&self, _: &[u8]) {}
    fn flush(&self) {}
}

static NIL: NilWriter = NilWriter;

static STDOUT: MutexCell<Stdout> = MutexCell::new(Stdout {
    writer: &NIL,
    clock: None,
    ring: Ring::Buffering(Vec::new()),
    line: LogBuffer::new(),
});

struct Stdout {
    writer: &'static StaticWriter,
    clock: Option<Clock>,
    ring: Ring,
    line: LogBuffer<LOG_MESSAGE_MAX>,
}

enum Ring {
    Buffering(Vec<Pending>),
    Attached(RingWriter<'static>),
    Off,
}

struct Pending {
    time_ns: u64,
    level: Level,
    kernel: bool,
    source: String,
    text: String,
}

struct FmtWriter<'a>(&'a StaticWriter);

impl core::fmt::Write for FmtWriter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.0.write_all(s.as_bytes());
        Ok(())
    }
}

impl Stdout {
    fn write(&mut self, level: Level, kernel: bool, source: &str, arguments: Arguments) {
        let time_ns = self.clock.as_ref().map_or(0, |clock| clock());
        let seconds = time_ns / NS_PER_SECOND;
        let millis = time_ns % NS_PER_SECOND / NS_PER_MILLISECOND;
        let prefix = prefix(level);
        let mut fmt = FmtWriter(self.writer);
        let _ = if source.is_empty() {
            writeln!(fmt, "[{seconds:5}.{millis:03}] {prefix}{arguments}\r")
        } else {
            writeln!(
                fmt,
                "[{seconds:5}.{millis:03}] {prefix}[{source}] {arguments}\r"
            )
        };
        self.writer.flush();

        self.line.clear();
        let _ = self.line.write_fmt(arguments);
        let text = self.line.as_str();
        match &mut self.ring {
            Ring::Attached(ring) => ring.push(&Record {
                time_ns,
                level,
                kernel,
                source,
                text,
            }),
            Ring::Buffering(pending) => {
                // Запись, под которую куча не дала памяти, теряется только для кольца.
                if let (Some(source), Some(text)) = (owned(source), owned(text))
                    && pending.try_reserve(1).is_ok()
                {
                    pending.push(Pending {
                        time_ns,
                        level,
                        kernel,
                        source,
                        text,
                    });
                }
            }
            Ring::Off => {}
        }
    }
}

/// Устанавливает writer для вывода логов.
/// Можно вызывать повторно, чтобы заменить writer.
pub fn set_stdout(w: &'static StaticWriter) {
    STDOUT.with_lock(|stdout| stdout.writer = w);
}

/// Ставит источник времени строк. Строки до вызова получают время 0.
pub fn set_clock(clock: Clock) {
    STDOUT.with_lock(|stdout| stdout.clock = Some(clock));
}

/// Подключает кольцо лога в словах `words` и переносит в него записи, накопленные до вызова.
/// Повторный вызов игнорируется.
pub fn attach_ring(words: &'static [AtomicU64]) {
    STDOUT.with_lock(|stdout| {
        let Ring::Buffering(pending) = &mut stdout.ring else {
            return;
        };
        let pending = core::mem::take(pending);
        let mut ring = RingWriter::new(words);

        for record in &pending {
            ring.push(&Record {
                time_ns: record.time_ns,
                level: record.level,
                kernel: record.kernel,
                source: &record.source,
                text: &record.text,
            });
        }
        stdout.ring = Ring::Attached(ring);
    });
}

/// Отказывается от кольца лога и освобождает записи, накопленные до вызова. Вызов после
/// [`attach_ring`] игнорируется.
pub fn detach_ring() {
    STDOUT.with_lock(|stdout| {
        if matches!(stdout.ring, Ring::Buffering(_)) {
            stdout.ring = Ring::Off;
        }
    });
}

/// Лог с префиксами времени, уровня и тега и переводом строки. В кольцо строка уходит с
/// признаком ядра и источником `tag`.
pub fn logf_tagged(lvl: Level, tag: &str, arguments: Arguments) {
    STDOUT.with_lock(|stdout| stdout.write(lvl, true, tag, arguments));
}

/// Строка userspace от источника `tag` с уровнем `Info`. В кольцо строка уходит без признака
/// ядра.
pub fn log_user(tag: &str, message: &str) {
    STDOUT.with_lock(|stdout| stdout.write(Level::Info, false, tag, format_args!("{message}")));
}

/// Возвращает имя файла без пути для использования в качестве тега.
pub fn file_tag(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn owned(text: &str) -> Option<String> {
    let mut owned = String::new();
    owned.try_reserve(text.len()).ok()?;
    owned.push_str(text);
    Some(owned)
}
