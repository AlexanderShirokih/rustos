//! Кольцо лога ядра в разделяемой памяти: раскладка, запись и чтение.
//!
//! Регион - слова `AtomicU64`: `head` и `tail`, позиции в байтах от начала лога, затем данные.
//! `tail` указывает на старейшую целую запись, `head` - на конец последней. Пишет только ядро,
//! читатели региона не меняют.
//!
//! Запись выровнена на слово: слово `[длина записи: u16][уровень][признак ядра][длина
//! источника][длина текста: u16][0]`, номер, время в наносекундах, затем байты источника и
//! текста. Числа записаны little-endian. Номера записей начинаются с нуля и растут на единицу.

use core::sync::atomic::{AtomicU64, Ordering, fence};

use ipc::wire::Cap;
use syscall::kind;

use crate::{LOG_MESSAGE_MAX, LOG_SOURCE_MAX, Level, chunks};

const WORD: usize = size_of::<u64>();
const HEAD: usize = 0;
const TAIL: usize = 1;
const HEADER_WORDS: usize = 2;
const RECORD_HEADER_BYTES: usize = 3 * WORD;
const PAYLOAD_MAX: usize = (LOG_SOURCE_MAX + LOG_MESSAGE_MAX).next_multiple_of(WORD);

/// Наибольшая длина записи в байтах.
pub const RECORD_MAX: usize = RECORD_HEADER_BYTES + PAYLOAD_MAX;

const _: () = assert!(
    RECORD_MAX <= u16::MAX as usize
        && LOG_SOURCE_MAX <= u8::MAX as usize
        && LOG_MESSAGE_MAX <= u16::MAX as usize,
    "the record header holds the record, source and text lengths"
);

ipc::wire_value! {
    /// Лог ядра: регион кольца записей и сигнал новых записей.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct LogRing<R = Cap<kind::MemoryRegion>, S = Cap<kind::Signal>> {
        /// Регион кольца ([`RingReader`]). Читатель отображает его на чтение и читает атомарными
        /// загрузками.
        pub region: R,
        /// Сигнал, поднимаемый после новых записей.
        pub signal: S,
    }
}

/// Источник слов региона кольца для чтения атомарными загрузками.
pub trait WordLoad {
    /// Число слов региона.
    fn word_count(&self) -> usize;

    /// Загружает слово `index` с порядком `Relaxed`. `None` - индекс вне региона.
    fn load(&self, index: usize) -> Option<u64>;
}

/// Строка лога.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record<'a> {
    /// Время записи от нуля системного счётчика в наносекундах.
    pub time_ns: u64,
    /// Уровень строки.
    pub level: Level,
    /// Строку записало ядро.
    pub kernel: bool,
    /// Источник строки.
    pub source: &'a str,
    /// Текст строки.
    pub text: &'a str,
}

/// Результат чтения кольца.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry<'a> {
    /// Запись под номером `number`.
    Record { number: u64, record: Record<'a> },
    /// `lost` записей перед следующей перезаписаны до чтения.
    Gap { lost: u64 },
}

/// Пишущая сторона кольца. Писатель сам нумерует записи и не ждёт читателей.
pub struct RingWriter<'a> {
    words: &'a [AtomicU64],
    head: u64,
    tail: u64,
    next_number: u64,
}

/// Читающая сторона кольца хранит позицию следующей записи и номер, который за ней ожидается.
#[derive(Debug, Default)]
pub struct RingReader {
    cursor: u64,
    expected: u64,
    pending: bool,
}

/// Копия записи, которую выдаёт [`RingReader::next`].
pub struct RecordBuf {
    payload: [u8; PAYLOAD_MAX],
    source_len: usize,
    text_len: usize,
    number: u64,
    time_ns: u64,
    level: Level,
    kernel: bool,
}

struct Publish<'w, 'a> {
    writer: &'w mut RingWriter<'a>,
    position: u64,
}

struct Window {
    head: u64,
    tail: u64,
}

enum Copied {
    Record(u64),
    Invalid,
}

impl WordLoad for [AtomicU64] {
    fn word_count(&self) -> usize {
        self.len()
    }

    fn load(&self, index: usize) -> Option<u64> {
        self.get(index).map(|word| word.load(Ordering::Relaxed))
    }
}

impl<'a> RingWriter<'a> {
    /// Размечает пустое кольцо в словах `words`.
    #[must_use]
    pub fn new(words: &'a [AtomicU64]) -> Self {
        if let (Some(head), Some(tail)) = (words.get(HEAD), words.get(TAIL)) {
            tail.store(0, Ordering::Relaxed);
            head.store(0, Ordering::Release);
        }
        Self {
            words,
            head: 0,
            tail: 0,
            next_number: 0,
        }
    }

    /// Пишет строку `record` следующим номером, вытесняя старейшие записи. Источник длиннее
    /// [`LOG_SOURCE_MAX`] байт и текст длиннее [`LOG_MESSAGE_MAX`] байт срезаются по границе
    /// символа.
    pub fn push(&mut self, record: &Record<'_>) {
        let source = chunks::<LOG_SOURCE_MAX>(record.source)
            .next()
            .unwrap_or_default();
        let text = chunks::<LOG_MESSAGE_MAX>(record.text)
            .next()
            .unwrap_or_default();
        let payload = source.len() + text.len();
        let len = record_len(payload);
        let capacity = capacity(self.words.len());
        if len > capacity {
            return;
        }
        let number = self.next_number;
        self.next_number = number.wrapping_add(1);

        // Длины проверены выше, поэтому запись не длиннее `RECORD_MAX`, а источник и текст в своих
        // границах.
        let [len_low, len_high] = u16::try_from(len).unwrap_or_default().to_le_bytes();
        let [text_low, text_high] = u16::try_from(text.len()).unwrap_or_default().to_le_bytes();
        let first = u64::from_le_bytes([
            len_low,
            len_high,
            record.level as u8,
            u8::from(record.kernel),
            u8::try_from(source.len()).unwrap_or_default(),
            text_low,
            text_high,
            0,
        ]);
        let header = [first, number, record.time_ns];
        let bytes = source.bytes().chain(text.bytes());
        let mut publish = self.begin(len, capacity);

        for word in header.into_iter().chain(payload_words(bytes, payload)) {
            publish.write(word);
        }
    }

    fn begin(&mut self, len: u64, capacity: u64) -> Publish<'_, 'a> {
        while self.head + len - self.tail > capacity {
            self.tail = match self.record_len_at(self.tail) {
                Some(evicted) if evicted > 0 && self.tail + evicted <= self.head => {
                    self.tail + evicted
                }
                _ => self.head,
            };
        }
        // Читатель сверяет `tail` после копии записи, и сдвиг виден ему раньше новых слов.
        // Барьер нужен каждой записи, потому что она пишет в слова, освобождённые прошлой.
        self.store(TAIL, self.tail, Ordering::Relaxed);
        fence(Ordering::Release);

        Publish {
            position: self.head,
            writer: self,
        }
    }

    fn record_len_at(&self, position: u64) -> Option<u64> {
        let word = self.words.load(data_index(position, self.words.len())?)?;
        Some(word & u64::from(u16::MAX))
    }

    fn store_data(&self, position: u64, value: u64) {
        if let Some(index) = data_index(position, self.words.len()) {
            self.store(index, value, Ordering::Relaxed);
        }
    }

    fn store(&self, index: usize, value: u64, order: Ordering) {
        if let Some(word) = self.words.get(index) {
            word.store(value, order);
        }
    }
}

impl RingReader {
    /// Читатель с курсором на начале лога. Первой он выдаёт старейшую сохранившуюся запись.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            cursor: 0,
            expected: 0,
            pending: false,
        }
    }

    /// Следующая запись кольца `words` либо разрыв перед ней. `None`, если все записи прочитаны.
    /// Отставший читатель переходит к старейшей записи и выдаёт разрыв с числом пропущенных.
    pub fn next<'b, W: WordLoad + ?Sized>(
        &mut self,
        words: &W,
        buf: &'b mut RecordBuf,
    ) -> Option<Entry<'b>> {
        if self.pending {
            self.pending = false;
            return Some(buf.entry());
        }

        loop {
            let Some(window) = Window::load(words) else {
                continue;
            };

            if !(window.tail..=window.head).contains(&self.cursor) {
                self.cursor = window.tail;
            }
            if self.cursor == window.head {
                return None;
            }

            let copied = buf.copy(words, self.cursor, window.head);
            if !Window::kept(words, self.cursor) {
                continue;
            }

            let Copied::Record(len) = copied else {
                self.cursor = window.head;
                return None;
            };
            self.cursor += len;
            if !buf.is_valid() {
                continue;
            }

            let lost = buf.number.saturating_sub(self.expected);
            self.expected = buf.number.wrapping_add(1);
            if lost > 0 {
                self.pending = true;
                return Some(Entry::Gap { lost });
            }
            return Some(buf.entry());
        }
    }

    /// Прочитаны ли все записи кольца `words`. После `next`, вернувшего `None`, новые записи
    /// видны по `false`.
    pub fn caught_up<W: WordLoad + ?Sized>(&self, words: &W) -> bool {
        !self.pending && words.load(HEAD) == Some(self.cursor)
    }
}

impl RecordBuf {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            payload: [0; PAYLOAD_MAX],
            source_len: 0,
            text_len: 0,
            number: 0,
            time_ns: 0,
            level: Level::Info,
            kernel: false,
        }
    }

    fn copy<W: WordLoad + ?Sized>(&mut self, words: &W, cursor: u64, head: u64) -> Copied {
        let count = words.word_count();
        let load = |position: u64| words.load(data_index(position, count)?);
        let (Some(first), Some(number), Some(time_ns)) = (
            load(cursor),
            load(cursor + WORD as u64),
            load(cursor + 2 * WORD as u64),
        ) else {
            return Copied::Invalid;
        };
        let [
            len_low,
            len_high,
            level,
            kernel,
            source_len,
            text_low,
            text_high,
            _,
        ] = first.to_le_bytes();
        let len = u64::from(u16::from_le_bytes([len_low, len_high]));
        let source_len = usize::from(source_len);
        let text_len = usize::from(u16::from_le_bytes([text_low, text_high]));
        let payload = source_len + text_len;
        if payload > PAYLOAD_MAX || len != record_len(payload) || len > head - cursor {
            return Copied::Invalid;
        }
        let start = cursor + RECORD_HEADER_BYTES as u64;
        for (index, bytes) in self
            .payload
            .as_chunks_mut::<WORD>()
            .0
            .iter_mut()
            .take(payload.div_ceil(WORD))
            .enumerate()
        {
            let Some(word) = load(start + (index * WORD) as u64) else {
                return Copied::Invalid;
            };
            *bytes = word.to_le_bytes();
        }
        self.source_len = source_len;
        self.text_len = text_len;
        self.number = number;
        self.time_ns = time_ns;
        self.kernel = kernel != 0;
        self.level = Level::from_raw(level).unwrap_or(Level::Info);
        Copied::Record(len)
    }

    fn source(&self) -> Option<&str> {
        core::str::from_utf8(self.payload.get(..self.source_len)?).ok()
    }

    fn text(&self) -> Option<&str> {
        let end = self.source_len + self.text_len;
        core::str::from_utf8(self.payload.get(self.source_len..end)?).ok()
    }

    fn is_valid(&self) -> bool {
        self.source().is_some() && self.text().is_some()
    }

    fn entry(&self) -> Entry<'_> {
        Entry::Record {
            number: self.number,
            record: Record {
                time_ns: self.time_ns,
                level: self.level,
                kernel: self.kernel,
                source: self.source().unwrap_or_default(),
                text: self.text().unwrap_or_default(),
            },
        }
    }
}

impl Publish<'_, '_> {
    fn write(&mut self, word: u64) {
        self.writer.store_data(self.position, word);
        self.position += WORD as u64;
    }
}

impl Drop for Publish<'_, '_> {
    fn drop(&mut self) {
        self.writer.head = self.position;
        self.writer.store(HEAD, self.position, Ordering::Release);
    }
}

impl Window {
    fn load<W: WordLoad + ?Sized>(words: &W) -> Option<Self> {
        // Регион отображён на чтение, и загрузки из него только `Relaxed`. Порядок с записью
        // ядра даёт барьер после загрузки `head`. `tail` дальше `head` означает, что ядро
        // вытеснило записи, опубликованные после загруженного `head`.
        let head = words.load(HEAD).unwrap_or_default();
        fence(Ordering::Acquire);
        let tail = words.load(TAIL).unwrap_or_default();
        (tail <= head).then_some(Self { head, tail })
    }

    fn kept<W: WordLoad + ?Sized>(words: &W, cursor: u64) -> bool {
        fence(Ordering::Acquire);
        words.load(TAIL).is_some_and(|tail| tail <= cursor)
    }
}

impl Default for RecordBuf {
    fn default() -> Self {
        Self::new()
    }
}

/// Число слов региона кольца, в данные которого помещается `bytes` байт.
#[must_use]
pub const fn ring_words(bytes: usize) -> usize {
    HEADER_WORDS + bytes.div_ceil(WORD)
}

fn record_len(payload: usize) -> u64 {
    (RECORD_HEADER_BYTES + payload.next_multiple_of(WORD)) as u64
}

fn capacity(words: usize) -> u64 {
    (words.saturating_sub(HEADER_WORDS) * WORD) as u64
}

fn data_index(position: u64, words: usize) -> Option<usize> {
    let data_words = u64::try_from(words.checked_sub(HEADER_WORDS)?).ok()?;
    let index = (position / WORD as u64).checked_rem(data_words)?;
    Some(HEADER_WORDS + usize::try_from(index).ok()?)
}

fn payload_words(bytes: impl Iterator<Item = u8>, len: usize) -> impl Iterator<Item = u64> {
    let mut bytes = bytes.fuse();
    (0..len.div_ceil(WORD)).map(move |_| {
        let mut word = [0u8; WORD];
        for (slot, byte) in word.iter_mut().zip(bytes.by_ref()) {
            *slot = byte;
        }
        u64::from_le_bytes(word)
    })
}

#[cfg(test)]
mod tests {
    use core::{
        cell::{Cell, RefCell},
        sync::atomic::{AtomicBool, AtomicU64, Ordering},
    };
    use std::{format, string::String, vec::Vec};

    use super::{
        Entry, RECORD_HEADER_BYTES, Record, RecordBuf, RingReader, RingWriter, TAIL, WordLoad,
        ring_words,
    };
    use crate::{LOG_MESSAGE_MAX, Level};

    #[derive(Debug, PartialEq, Eq)]
    enum Read {
        Record(u64, String),
        Gap(u64),
    }

    fn region(bytes: usize) -> Vec<AtomicU64> {
        (0..ring_words(bytes)).map(|_| AtomicU64::new(0)).collect()
    }

    fn info(text: &str) -> Record<'_> {
        Record {
            time_ns: 0,
            level: Level::Info,
            kernel: false,
            source: "src",
            text,
        }
    }

    fn record_bytes(text: &str) -> usize {
        RECORD_HEADER_BYTES + ("src".len() + text.len()).next_multiple_of(8)
    }

    fn drain<W: WordLoad + ?Sized>(words: &W, reader: &mut RingReader) -> Vec<Read> {
        let mut buf = RecordBuf::new();
        let mut read = Vec::new();
        while let Some(entry) = reader.next(words, &mut buf) {
            read.push(match entry {
                Entry::Record { number, record } => Read::Record(number, record.text.into()),
                Entry::Gap { lost } => Read::Gap(lost),
            });
        }
        read
    }

    #[test]
    fn two_records_are_read_in_order_with_their_fields() {
        let words = region(256);
        let mut writer = RingWriter::new(&words);
        let first = Record {
            time_ns: 1_500_000_000,
            level: Level::Debug,
            kernel: true,
            source: "service.rs",
            text: "IRQ 33 bound",
        };
        writer.push(&first);
        writer.push(&info("second"));

        let mut reader = RingReader::new();
        let mut buf = RecordBuf::new();
        assert_eq!(
            reader.next(&words[..], &mut buf),
            Some(Entry::Record {
                number: 0,
                record: first
            })
        );
        assert_eq!(
            reader.next(&words[..], &mut buf),
            Some(Entry::Record {
                number: 1,
                record: info("second")
            })
        );
        assert_eq!(reader.next(&words[..], &mut buf), None);
        assert!(reader.caught_up(&words[..]));
    }

    #[test]
    fn a_record_of_the_message_bound_is_read_whole() {
        // Граница слова приходится на середину одного из двухбайтовых символов.
        let text = "\u{451}".repeat(LOG_MESSAGE_MAX / 2);
        let words = region(2 * record_bytes(&text));
        RingWriter::new(&words).push(&info(&text));

        assert_eq!(
            drain(&words[..], &mut RingReader::new()),
            [Read::Record(0, text)]
        );
    }

    #[test]
    fn a_record_across_the_end_of_the_data_is_read_whole() {
        // Данные не кратны длине записи, и четвёртая запись переходит через конец данных.
        let words = region(3 * record_bytes("0123456789") + 8);
        let mut writer = RingWriter::new(&words);
        let mut reader = RingReader::new();
        for number in 0..5 {
            let text = format!("{number:010}");
            writer.push(&info(&text));
            assert_eq!(drain(&words[..], &mut reader), [Read::Record(number, text)]);
        }
    }

    #[test]
    fn records_beyond_the_capacity_give_a_gap_then_the_kept_records() {
        let words = region(4 * record_bytes("0"));
        let mut writer = RingWriter::new(&words);
        for number in 0..10 {
            writer.push(&info(&format!("{number}")));
        }

        assert_eq!(
            drain(&words[..], &mut RingReader::new()),
            [
                Read::Gap(6),
                Read::Record(6, "6".into()),
                Read::Record(7, "7".into()),
                Read::Record(8, "8".into()),
                Read::Record(9, "9".into()),
            ]
        );
    }

    struct Evicting<'a> {
        words: &'a [AtomicU64],
        second: u64,
        evicted: AtomicBool,
    }

    impl WordLoad for Evicting<'_> {
        fn word_count(&self) -> usize {
            self.words.len()
        }

        fn load(&self, index: usize) -> Option<u64> {
            if index == super::HEADER_WORDS + 3 && !self.evicted.swap(true, Ordering::Relaxed) {
                self.words.get(TAIL)?.store(self.second, Ordering::Relaxed);
            }
            self.words.load(index)
        }
    }

    struct Overtaking<'a> {
        words: &'a [AtomicU64],
        writer: RefCell<RingWriter<'a>>,
        overtaken: Cell<bool>,
    }

    impl WordLoad for Overtaking<'_> {
        fn word_count(&self) -> usize {
            self.words.len()
        }

        fn load(&self, index: usize) -> Option<u64> {
            if index == TAIL && !self.overtaken.replace(true) {
                let mut writer = self.writer.borrow_mut();
                for number in 2..12 {
                    writer.push(&info(&format!("{number}")));
                }
            }
            self.words.load(index)
        }
    }

    #[test]
    fn a_record_evicted_during_the_copy_is_not_read() {
        let words = region(256);
        let mut writer = RingWriter::new(&words);
        writer.push(&info("0"));
        writer.push(&info("1"));
        let evicting = Evicting {
            words: &words,
            second: record_bytes("0") as u64,
            evicted: AtomicBool::new(false),
        };

        assert_eq!(
            drain(&evicting, &mut RingReader::new()),
            [Read::Gap(1), Read::Record(1, "1".into())]
        );
    }

    // Писатель обогнал читателя между загрузками `head` и `tail`, и `tail` ушёл дальше `head`.
    #[test]
    fn a_tail_past_the_loaded_head_is_reloaded() {
        let words = region(256);
        let mut writer = RingWriter::new(&words);
        writer.push(&info("0"));
        writer.push(&info("1"));
        let overtaking = Overtaking {
            words: &words,
            writer: RefCell::new(writer),
            overtaken: Cell::new(false),
        };

        let read = drain(&overtaking, &mut RingReader::new());
        let numbers: Vec<u64> = read
            .iter()
            .filter_map(|entry| match entry {
                Read::Record(number, _) => Some(*number),
                Read::Gap(_) => None,
            })
            .collect();
        assert!(matches!(read.first(), Some(Read::Gap(_))));
        assert_eq!(numbers.last(), Some(&11));
        assert!(numbers.windows(2).all(|pair| pair[1] == pair[0] + 1));
    }
}
