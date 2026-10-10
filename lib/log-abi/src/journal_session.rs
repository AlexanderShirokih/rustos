//! Контракт [`journal`] подписки на строки системного журнала, кадры её кольца и их сборка.
//!
//! Подписка выдаёт регион кольца `SpscRing` со слотами по [`JOURNAL_SLOT_PAYLOAD`] байт и
//! сигнал новых кадров. Кадр - `[вид][номер][длина строки][длина источника][источник][кусок
//! текста]`, числа записаны little-endian. Строка идёт кадром `Line` с источником и длиной всего
//! текста и продолжениями без них. Кадр `Gap` содержит в поле номера число строк, не попавших в
//! кольцо.

use ipc::wire::Cap;
use syscall::kind;

use crate::{LOG_MESSAGE_MAX, LOG_SOURCE_MAX, LogBuffer, chunks};

/// Полезная ёмкость слота кольца подписки.
pub const JOURNAL_SLOT_PAYLOAD: usize = 256;

/// Число слотов кольца подписки.
pub const JOURNAL_RING_SLOTS: usize = 128;

const KIND_LINE: u8 = 1;
const KIND_CONTINUATION: u8 = 2;
const KIND_GAP: u8 = 3;

const NUMBER_BYTES: usize = size_of::<u64>();
const LENGTH_BYTES: usize = size_of::<u16>();
const FRAME_HEADER: usize = 1 + NUMBER_BYTES + LENGTH_BYTES + 1;

const CHUNK_MAX: usize = JOURNAL_SLOT_PAYLOAD - FRAME_HEADER - LOG_SOURCE_MAX;

const _: () = assert!(
    LOG_SOURCE_MAX <= u8::MAX as usize
        && LOG_MESSAGE_MAX <= u16::MAX as usize
        && CHUNK_MAX >= char::MAX.len_utf8(),
    "a journal frame holds the whole source, the line length and a scalar of the text"
);

ipc::wire_enum! {
    /// Отказ в подписке на журнал.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum JournalError {
        /// Подписка не выдана.
        Refused = 1,
    }
}

ipc::wire_value! {
    /// Кольцо подписки на журнал.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Subscription<R = Cap<kind::MemoryRegion>, S = Cap<kind::Signal>> {
        /// Регион кольца `SpscRing` со слотами по [`JOURNAL_SLOT_PAYLOAD`] байт. Потребитель
        /// отображает его целиком на чтение и запись.
        pub region: R,
        /// Сигнал, которым журнал будит потребителя пустого кольца.
        pub signal: S,
    }
}

ipc::protocol! {
    /// Подписка на строки системного журнала.
    pub mod journal {
        name = "Journal";

        /// Выдаёт кольцо с последними строками журнала и каждой новой строкой. Не поместившиеся
        /// в кольцо строки отмечаются кадром `Gap`. Incarnation получает одну подписку, её регион
        /// и сигнал действуют до конца incarnation.
        call fn subscribe() -> own Result<Subscription, JournalError>;
    }
}

/// Кадры одной строки журнала для кольца подписки.
#[derive(Debug, Clone, Copy)]
pub struct LineFrames<'a> {
    number: u64,
    source: &'a str,
    text: &'a str,
    count: usize,
}

/// Строка журнала, собранная из кадров кольца, либо отметка разрыва.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalEntry<'a> {
    /// Строка `text` источника `source` под номером `number`.
    Line {
        number: u64,
        source: &'a str,
        text: &'a str,
    },
    /// `lost` строк перед следующей не попали в кольцо.
    Gap { lost: u64 },
}

/// Сборщик строк журнала из кадров кольца подписки.
pub struct JournalAssembler {
    number: Option<u64>,
    length: usize,
    source: LogBuffer<LOG_SOURCE_MAX>,
    text: LogBuffer<LOG_MESSAGE_MAX>,
}

struct Frame<'a> {
    kind: u8,
    number: u64,
    length: u16,
    source: &'a str,
    chunk: &'a str,
}

impl<'a> LineFrames<'a> {
    /// Строка `text` источника `source` под номером `number`. Источник длиннее
    /// [`LOG_SOURCE_MAX`] байт и текст длиннее [`LOG_MESSAGE_MAX`] байт срезаются по границе
    /// символа.
    #[must_use]
    pub fn new(number: u64, source: &'a str, text: &'a str) -> Self {
        let source = chunks::<LOG_SOURCE_MAX>(source).next().unwrap_or_default();
        let text = chunks::<LOG_MESSAGE_MAX>(text).next().unwrap_or_default();
        Self {
            number,
            source,
            text,
            count: chunks::<CHUNK_MAX>(text).count(),
        }
    }

    /// Число кадров строки.
    #[must_use]
    pub fn count(&self) -> usize {
        self.count
    }

    /// Отдаёт кадры строки по порядку в `emit` и останавливается на первом его отказе.
    pub fn try_for_each<E>(&self, mut emit: impl FnMut(&[u8]) -> Result<(), E>) -> Result<(), E> {
        let mut frame = [0u8; JOURNAL_SLOT_PAYLOAD];
        // Текст срезан до `LOG_MESSAGE_MAX`, и его длина помещается в `u16`.
        let mut length = u16::try_from(self.text.len()).unwrap_or_default();
        let mut kind = KIND_LINE;
        let mut source = self.source;
        for piece in chunks::<CHUNK_MAX>(self.text) {
            let len = encode(&mut frame, kind, self.number, length, source, piece);
            emit(frame.get(..len).unwrap_or_default())?;
            kind = KIND_CONTINUATION;
            length = 0;
            source = "";
        }
        Ok(())
    }
}

impl JournalAssembler {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            number: None,
            length: 0,
            source: LogBuffer::new(),
            text: LogBuffer::new(),
        }
    }

    /// Разбирает кадр `frame`. Строка возвращается, когда набран текст длины из её кадра `Line`,
    /// разрыв - сразу. Продолжение без начала, повреждённый кадр и текст длиннее объявленного
    /// отбрасываются вместе с начатой строкой.
    pub fn push(&mut self, frame: &[u8]) -> Option<JournalEntry<'_>> {
        let Some(frame) = decode(frame) else {
            self.number = None;
            return None;
        };
        match frame.kind {
            KIND_GAP => {
                self.number = None;
                return Some(JournalEntry::Gap { lost: frame.number });
            }
            KIND_LINE => {
                self.source.clear();
                self.source.push_str(frame.source);
                self.text.clear();
                self.length = usize::from(frame.length);
                self.number = Some(frame.number);
            }
            KIND_CONTINUATION if self.number == Some(frame.number) => {}
            _ => {
                self.number = None;
                return None;
            }
        }
        self.text.push_str(frame.chunk);
        if self.text.as_str().len() < self.length {
            return None;
        }
        let number = self.number.take()?;
        if self.text.as_str().len() > self.length {
            return None;
        }
        Some(JournalEntry::Line {
            number,
            source: self.source.as_str(),
            text: self.text.as_str(),
        })
    }
}

impl Default for JournalAssembler {
    fn default() -> Self {
        Self::new()
    }
}

/// Кадр разрыва: `lost` строк не попали в кольцо.
#[must_use]
pub fn gap_frame(lost: u64) -> [u8; FRAME_HEADER] {
    let mut frame = [0u8; FRAME_HEADER];
    encode(&mut frame, KIND_GAP, lost, 0, "", "");
    frame
}

fn encode(
    frame: &mut [u8],
    kind: u8,
    number: u64,
    length: u16,
    source: &str,
    chunk: &str,
) -> usize {
    // Источник срезан до `LOG_SOURCE_MAX`, и его длина помещается в байт.
    let source_len = u8::try_from(source.len()).unwrap_or_default();
    let parts: [&[u8]; 6] = [
        &[kind],
        &number.to_le_bytes(),
        &length.to_le_bytes(),
        &[source_len],
        source.as_bytes(),
        chunk.as_bytes(),
    ];
    let mut len = 0;
    for part in parts {
        let Some(target) = frame.get_mut(len..len + part.len()) else {
            break;
        };
        target.copy_from_slice(part);
        len += part.len();
    }
    len
}

fn decode(frame: &[u8]) -> Option<Frame<'_>> {
    let (&kind, rest) = frame.split_first()?;
    let (number, rest) = rest.split_first_chunk::<NUMBER_BYTES>()?;
    let (length, rest) = rest.split_first_chunk::<LENGTH_BYTES>()?;
    let (&source_len, rest) = rest.split_first()?;
    let (source, chunk) = rest.split_at_checked(usize::from(source_len))?;
    Some(Frame {
        kind,
        number: u64::from_le_bytes(*number),
        length: u16::from_le_bytes(*length),
        source: core::str::from_utf8(source).ok()?,
        chunk: core::str::from_utf8(chunk).ok()?,
    })
}

#[cfg(test)]
mod tests {
    use std::{string::String, thread, vec::Vec};

    use ipc::{CallError, IntoWireValue, wire::Cap};
    use ipc_test::{MockHandle, MockResources};
    use syscall::kind;

    use super::{
        JOURNAL_SLOT_PAYLOAD, JournalAssembler, JournalEntry, JournalError, LineFrames,
        Subscription, journal,
    };
    use crate::LOG_MESSAGE_MAX;

    fn frames_of(number: u64, source: &str, text: &str) -> Vec<Vec<u8>> {
        let line = LineFrames::new(number, source, text);
        let mut frames = Vec::new();
        line.try_for_each(|frame| {
            frames.push(frame.to_vec());
            Ok::<_, ()>(())
        })
        .expect("frames are emitted");
        assert_eq!(frames.len(), line.count());
        frames
    }

    fn assembled(frames: &[Vec<u8>]) -> Vec<(u64, String, String)> {
        let mut assembler = JournalAssembler::new();
        let mut lines = Vec::new();
        for frame in frames {
            assert!(frame.len() <= JOURNAL_SLOT_PAYLOAD);
            if let Some(JournalEntry::Line {
                number,
                source,
                text,
            }) = assembler.push(frame)
            {
                lines.push((number, String::from(source), String::from(text)));
            }
        }
        lines
    }

    #[test]
    fn a_line_of_the_message_bound_assembles_whole() {
        // Граница куска приходится на середину одного из двухбайтовых символов.
        let text = "\u{451}".repeat(LOG_MESSAGE_MAX / 2);
        let frames = frames_of(1, "source", &text);
        assert!(frames.len() > 1);
        assert_eq!(assembled(&frames), [(1, "source".into(), text)]);
    }

    #[test]
    fn a_continuation_without_its_start_is_dropped() {
        let long = frames_of(3, "lost", &"x".repeat(LOG_MESSAGE_MAX));
        let mut frames: Vec<Vec<u8>> = long[1..].to_vec();
        frames.extend(frames_of(4, "next", "after"));
        assert_eq!(assembled(&frames), [(4, "next".into(), "after".into())]);
    }

    struct Subscriptions {
        granted: bool,
    }

    impl journal::Service<MockResources> for Subscriptions {
        fn subscribe(
            &mut self,
            _frame: &MockResources,
        ) -> impl IntoWireValue<
            MockResources,
            Read = Result<Subscription<Cap<kind::MemoryRegion>, Cap<kind::Signal>>, JournalError>,
        > {
            if self.granted {
                return Err(JournalError::Refused);
            }
            self.granted = true;
            Ok(Subscription {
                region: MockHandle::<kind::MemoryRegion>::new(0x41),
                signal: MockHandle::<kind::Signal>::new(0x42),
            })
        }
    }

    #[test]
    fn subscribe_round_trips_the_ring_and_the_refusal() {
        let (client_end, server_end) = ipc_test::pair();
        let client = journal::Client::new(client_end);
        let mut service = Subscriptions { granted: false };
        thread::scope(|scope| {
            scope.spawn(|| {
                server_end
                    .serve_all(|frame| journal::dispatch(&mut service, frame))
                    .expect("dispatch ok");
            });
            assert_eq!(
                client.subscribe(),
                Ok(Subscription {
                    region: MockHandle::new(0x41),
                    signal: MockHandle::new(0x42),
                })
            );
            assert_eq!(
                client.subscribe(),
                Err(CallError::Remote(JournalError::Refused))
            );
            drop(client);
        });
    }
}
