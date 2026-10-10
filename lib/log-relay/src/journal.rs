//! Журнал строк relay: история последних строк и кольца подписчиков.
//!
//! Relay не ждёт подписчика. Строка кладётся в кольцо целиком либо считается пропущенной, и
//! перед следующей доставленной строкой подписчик получает кадр `Gap` с числом пропусков.
//! Подписка кладёт в новое кольцо самые новые строки истории, которые в него помещаются.
//! Подписчик получает историю и следующие за ней строки без пропуска и повтора на их границе.

use alloc::{boxed::Box, collections::VecDeque, string::String, sync::Arc, vec::Vec};

use io::{RingError, SpscRing};
use ipc::wire::IpcError;
use kernel_objects::RingProducer;
use log_abi::{JOURNAL_RING_SLOTS, JOURNAL_SLOT_PAYLOAD, JournalError, LineFrames, gap_frame};
use runtime::Mutex;

use crate::relay::LogSink;

const JOURNAL_HISTORY_BYTES: usize = 64 * 1024;

/// Размер региона кольца подписки: кольцо из [`JOURNAL_RING_SLOTS`] слотов, округлённое до
/// страницы.
pub const JOURNAL_REGION_BYTES: u64 =
    SpscRing::region_bytes(JOURNAL_RING_SLOTS, JOURNAL_SLOT_PAYLOAD)
        .next_multiple_of(syscall::PAGE_SIZE) as u64;

/// Кольцо подписчика, в которое журнал кладёт кадры.
pub trait JournalRing: Send {
    /// Число кадров, которые поместятся в кольцо сейчас.
    fn free_slots(&self) -> usize;

    /// Кладёт кадр в кольцо.
    fn push(&self, frame: &[u8]) -> Result<(), RingError>;
}

/// Строка истории журнала.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalLine {
    /// Номер строки. Растёт на единицу с каждой строкой журнала.
    pub number: u64,
    /// Источник строки.
    pub source: String,
    /// Текст строки.
    pub text: String,
}

/// Журнал строк relay. Копии значения делят одну историю и один список подписчиков.
#[derive(Clone, Default)]
pub struct Journal {
    state: Arc<Mutex<State>>,
}

/// Сток, передающий строку стоку `S` и затем записывающий её в журнал. Строка попадает в журнал
/// и при отказе `S`.
pub struct JournalSink<S> {
    journal: Journal,
    next: S,
}

#[derive(Default)]
struct State {
    next_number: u64,
    history: VecDeque<JournalLine>,
    history_bytes: usize,
    subscribers: Vec<Subscriber>,
}

struct Subscriber {
    key: u64,
    ring: Box<dyn JournalRing>,
    lost: u64,
}

impl JournalRing for RingProducer {
    fn free_slots(&self) -> usize {
        Self::free_slots(self)
    }

    fn push(&self, frame: &[u8]) -> Result<(), RingError> {
        Self::push(self, frame)
    }
}

impl Journal {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Записывает строку `text` источника `source` в историю и в кольцо каждого подписчика.
    pub fn record(&self, source: &str, text: &str) {
        let mut state = self.state.lock();
        let number = state.next_number;
        state.next_number = number.wrapping_add(1);

        let frames = LineFrames::new(number, source, text);
        for subscriber in &mut state.subscribers {
            subscriber.deliver(&frames);
        }

        let line = JournalLine {
            number,
            source: String::from(source),
            text: String::from(text),
        };
        state.history_bytes += line_bytes(&line);
        state.history.push_back(line);
        while state.history_bytes > JOURNAL_HISTORY_BYTES {
            let Some(oldest) = state.history.pop_front() else {
                break;
            };
            state.history_bytes -= line_bytes(&oldest);
        }
    }

    /// Подписывает кольцо `ring` под ключом `key`. Кольцо сначала получает самые новые строки
    /// истории, которые в него помещаются, а перед ними `Gap` с числом остальных. `Refused` -
    /// нет памяти под запись подписчика.
    pub fn subscribe(
        &self,
        key: u64,
        ring: impl JournalRing + 'static,
    ) -> Result<(), JournalError> {
        // История копируется под тем же замком, что и запись строки, и граница не теряет строк.
        let mut state = self.state.lock();
        state
            .subscribers
            .try_reserve(1)
            .map_err(|_| JournalError::Refused)?;

        // Один слот остаётся под `Gap` перед историей, не поместившейся целиком.
        let free = ring.free_slots();
        let total: usize = state
            .history
            .iter()
            .map(|line| frames_of(line).count())
            .sum();
        let budget = if total <= free {
            free
        } else {
            free.saturating_sub(1)
        };
        let mut used = 0;
        let fitting = state
            .history
            .iter()
            .rev()
            .take_while(|line| {
                used += frames_of(line).count();
                used <= budget
            })
            .count();
        let skipped = state.history.len() - fitting;

        let mut subscriber = Subscriber {
            key,
            ring: Box::new(ring),
            lost: u64::try_from(skipped).unwrap_or(u64::MAX),
        };
        for line in state.history.iter().skip(skipped) {
            subscriber.deliver(&frames_of(line));
        }
        state.subscribers.push(subscriber);
        Ok(())
    }

    /// Снимает подписку с ключом `key`, если она есть. Её кольцо больше строк не получает.
    pub fn unsubscribe(&self, key: u64) {
        // Кольцо освобождается после снятия замка, и снятие отображения не задерживает relay.
        let _removed = {
            let mut state = self.state.lock();
            let position = state
                .subscribers
                .iter()
                .position(|subscriber| subscriber.key == key);
            position.map(|position| state.subscribers.swap_remove(position))
        };
    }

    /// Снимок истории в порядке номеров.
    #[must_use]
    pub fn lines(&self) -> Vec<JournalLine> {
        self.state.lock().history.iter().cloned().collect()
    }
}

impl<S: LogSink> JournalSink<S> {
    /// Сток, пишущий строки в `next` и затем в `journal`.
    pub fn new(journal: Journal, next: S) -> Self {
        Self { journal, next }
    }
}

impl<S: LogSink> LogSink for JournalSink<S> {
    fn write(&mut self, source: &str, message: &str) -> Result<(), IpcError> {
        let written = self.next.write(source, message);
        self.journal.record(source, message);
        written
    }
}

impl Subscriber {
    fn deliver(&mut self, frames: &LineFrames<'_>) {
        let gap = usize::from(self.lost > 0);
        if self.ring.free_slots() < gap + frames.count() {
            self.lost = self.lost.saturating_add(1);
            return;
        }
        if gap > 0 && self.ring.push(&gap_frame(self.lost)).is_err() {
            self.lost = self.lost.saturating_add(1);
            return;
        }
        self.lost = 0;
        // Кольцо пишет только этот поток, и место под кадры строки проверено выше. Отказ здесь
        // означает испорченный подписчиком заголовок, и строка считается пропущенной.
        if frames.try_for_each(|frame| self.ring.push(frame)).is_err() {
            self.lost = 1;
        }
    }
}

fn line_bytes(line: &JournalLine) -> usize {
    size_of::<JournalLine>() + line.source.len() + line.text.len()
}

fn frames_of(line: &JournalLine) -> LineFrames<'_> {
    LineFrames::new(line.number, &line.source, &line.text)
}

#[cfg(test)]
mod tests {
    use alloc::{
        string::{String, ToString},
        vec::Vec,
    };
    use std::sync::{Arc, Mutex as StdMutex};

    use io::RingError;
    use ipc::wire::IpcError;
    use log_abi::{JournalAssembler, JournalEntry, LOG_MESSAGE_MAX};

    use super::{JOURNAL_HISTORY_BYTES, Journal, JournalRing, JournalSink, line_bytes};
    use crate::relay::LogSink;

    #[derive(Debug, PartialEq, Eq)]
    enum Read {
        Line(u64, String, String),
        Gap(u64),
    }

    #[derive(Clone)]
    struct TestRing {
        capacity: usize,
        frames: Arc<StdMutex<Vec<Vec<u8>>>>,
    }

    impl TestRing {
        fn new(capacity: usize) -> Self {
            Self {
                capacity,
                frames: Arc::default(),
            }
        }

        fn drain(&self) -> Vec<Read> {
            let frames = core::mem::take(&mut *self.frames.lock().expect("ring lock"));
            let mut assembler = JournalAssembler::new();
            frames
                .iter()
                .filter_map(|frame| match assembler.push(frame)? {
                    JournalEntry::Line {
                        number,
                        source,
                        text,
                    } => Some(Read::Line(number, source.to_string(), text.to_string())),
                    JournalEntry::Gap { lost } => Some(Read::Gap(lost)),
                })
                .collect()
        }
    }

    impl JournalRing for TestRing {
        fn free_slots(&self) -> usize {
            self.capacity - self.frames.lock().expect("ring lock").len()
        }

        fn push(&self, frame: &[u8]) -> Result<(), RingError> {
            let mut frames = self.frames.lock().map_err(|_| RingError::Full)?;
            if frames.len() == self.capacity {
                return Err(RingError::Full);
            }
            frames.push(frame.to_vec());
            Ok(())
        }
    }

    fn line(number: u64, text: &str) -> Read {
        Read::Line(number, "source".to_string(), text.to_string())
    }

    fn record_texts(journal: &Journal, texts: &[&str]) {
        for text in texts {
            journal.record("source", text);
        }
    }

    #[test]
    fn a_new_subscriber_reads_the_history_then_the_new_lines() {
        let journal = Journal::new();
        record_texts(&journal, &["before", "also before"]);
        let ring = TestRing::new(16);
        journal.subscribe(1, ring.clone()).expect("subscribed");
        journal.record("source", "after");

        assert_eq!(
            ring.drain(),
            [line(0, "before"), line(1, "also before"), line(2, "after")]
        );
    }

    #[test]
    fn history_filling_the_ring_exactly_has_no_gap() {
        let journal = Journal::new();
        record_texts(&journal, &["0", "1"]);
        let ring = TestRing::new(2);
        journal.subscribe(1, ring.clone()).expect("subscribed");

        assert_eq!(ring.drain(), [line(0, "0"), line(1, "1")]);
    }

    #[test]
    fn history_beyond_the_ring_starts_with_a_gap() {
        let journal = Journal::new();
        record_texts(&journal, &["0", "1", "2", "3", "4", "5"]);
        let ring = TestRing::new(4);
        journal.subscribe(1, ring.clone()).expect("subscribed");

        assert_eq!(
            ring.drain(),
            [Read::Gap(3), line(3, "3"), line(4, "4"), line(5, "5")]
        );
    }

    #[test]
    fn a_full_ring_loses_whole_lines_and_marks_the_gap_before_the_next() {
        let journal = Journal::new();
        let ring = TestRing::new(4);
        journal.subscribe(1, ring.clone()).expect("subscribed");
        record_texts(&journal, &["0", "1", "2", "3", "4", "5"]);
        let long = "l".repeat(LOG_MESSAGE_MAX);
        journal.record("source", &long);

        assert_eq!(
            ring.drain(),
            [line(0, "0"), line(1, "1"), line(2, "2"), line(3, "3")]
        );
        journal.record("source", "after");
        assert_eq!(ring.drain(), [Read::Gap(3), line(7, "after")]);
    }

    #[test]
    fn a_full_ring_does_not_hold_back_another_subscriber() {
        let journal = Journal::new();
        let stalled = TestRing::new(1);
        let reading = TestRing::new(16);
        journal.subscribe(1, stalled).expect("subscribed");
        journal.subscribe(2, reading.clone()).expect("subscribed");
        record_texts(&journal, &["0", "1", "2"]);

        assert_eq!(reading.drain(), [line(0, "0"), line(1, "1"), line(2, "2")]);
    }

    #[test]
    fn an_unsubscribed_ring_receives_no_more_lines() {
        let journal = Journal::new();
        let ring = TestRing::new(16);
        journal.subscribe(1, ring.clone()).expect("subscribed");
        journal.unsubscribe(1);
        journal.record("source", "after");

        assert_eq!(ring.drain(), []);
    }

    #[test]
    fn the_history_keeps_the_newest_lines_within_its_bound() {
        let journal = Journal::new();
        let text = "h".repeat(1000);
        for _ in 0..2 * JOURNAL_HISTORY_BYTES / text.len() {
            journal.record("source", &text);
        }
        journal.record("source", "last");

        let lines = journal.lines();
        let kept: usize = lines.iter().map(line_bytes).sum();
        assert!(kept <= JOURNAL_HISTORY_BYTES);
        assert!(
            lines
                .windows(2)
                .all(|pair| pair[1].number == pair[0].number + 1)
        );
        assert_eq!(lines.last().map(|line| line.text.as_str()), Some("last"));
    }

    struct Refusing;

    impl LogSink for Refusing {
        fn write(&mut self, _source: &str, _message: &str) -> Result<(), IpcError> {
            Err(IpcError::PeerClosed)
        }
    }

    #[test]
    fn a_refusing_next_sink_still_leaves_the_line_in_the_journal() {
        let journal = Journal::new();
        let mut sink = JournalSink::new(journal.clone(), Refusing);
        assert_eq!(sink.write("source", "kept"), Err(IpcError::PeerClosed));

        let lines = journal.lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines.first().map(|line| line.text.as_str()), Some("kept"));
    }
}
