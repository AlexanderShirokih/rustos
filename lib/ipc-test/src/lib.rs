//! Host-mock транспорта IPC поверх [`Transport`] для тестирования контрактов.

use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex},
};

use ipc::{MessageLen, Transport, wire::IpcError};

/// Один кадр в очереди.
struct Frame {
    bytes: Vec<u8>,
    handles: Vec<u32>,
}

/// Очередь одного направления.
struct Pipe {
    frames: VecDeque<Frame>,
    sender_open: bool,
    receiver_open: bool,
}

/// Очередь с переменной ожидания для блокирующего чтения.
struct Shared {
    pipe: Mutex<Pipe>,
    readable: Condvar,
}

impl Shared {
    fn open() -> Arc<Self> {
        Arc::new(Self {
            pipe: Mutex::new(Pipe {
                frames: VecDeque::new(),
                sender_open: true,
                receiver_open: true,
            }),
            readable: Condvar::new(),
        })
    }
}

/// Один конец host-mock канала: пишет в `outbox`, читает из `inbox`.
pub struct MockEnd {
    outbox: Arc<Shared>,
    inbox: Arc<Shared>,
}

impl MockEnd {
    /// Создаёт связанную пару концов: запись на одном видна в чтении другого.
    #[must_use]
    pub fn pair() -> (MockEnd, MockEnd) {
        let ab = Shared::open();
        let ba = Shared::open();
        let a = MockEnd {
            outbox: Arc::clone(&ab),
            inbox: Arc::clone(&ba),
        };
        let b = MockEnd {
            outbox: ba,
            inbox: ab,
        };
        (a, b)
    }
}

impl Drop for MockEnd {
    fn drop(&mut self) {
        self.outbox
            .pipe
            .lock()
            .expect("mutex not poisoned")
            .sender_open = false;
        self.outbox.readable.notify_all();
        self.inbox
            .pipe
            .lock()
            .expect("mutex not poisoned")
            .receiver_open = false;
    }
}

impl Transport for MockEnd {
    fn write_message(&self, bytes: &[u8], handles: &[u32]) -> Result<(), IpcError> {
        let mut pipe = self.outbox.pipe.lock().expect("mutex not poisoned");
        if !pipe.receiver_open {
            return Err(IpcError::PeerClosed);
        }
        pipe.frames.push_back(Frame {
            bytes: bytes.to_vec(),
            handles: handles.to_vec(),
        });
        drop(pipe);
        self.outbox.readable.notify_all();
        Ok(())
    }

    fn read_message(&self, bytes: &mut [u8], handles: &mut [u32]) -> Result<MessageLen, IpcError> {
        let mut pipe = self.inbox.pipe.lock().expect("mutex not poisoned");
        let Some(frame) = pipe.frames.pop_front() else {
            return if pipe.sender_open {
                Err(IpcError::WouldBlock)
            } else {
                Err(IpcError::PeerClosed)
            };
        };
        if frame.bytes.len() > bytes.len() || frame.handles.len() > handles.len() {
            // Кадр не помещается: вернуть его в очередь, не теряя.
            pipe.frames.push_front(frame);
            return Err(IpcError::Truncated);
        }
        bytes[..frame.bytes.len()].copy_from_slice(&frame.bytes);
        handles[..frame.handles.len()].copy_from_slice(&frame.handles);
        Ok(MessageLen::new(frame.bytes.len(), frame.handles.len()))
    }

    fn wait_readable(&self, timeout_ns: u64) -> Result<(), IpcError> {
        // Ненулевой timeout трактуется как ожидание до готовности или закрытия.
        let mut pipe = self.inbox.pipe.lock().expect("mutex not poisoned");
        loop {
            if !pipe.frames.is_empty() {
                return Ok(());
            }
            if !pipe.sender_open {
                return Err(IpcError::PeerClosed);
            }
            if timeout_ns == 0 {
                return Err(IpcError::Timeout);
            }
            pipe = self.inbox.readable.wait(pipe).expect("condvar wait");
        }
    }
}

#[cfg(test)]
mod tests {
    use ipc::{
        MessageLen, Transport,
        wire::{
            FieldCursor, HEADER_SIZE, Header, IpcError, MESSAGE_INLINE_MAX, MessageBuf,
            value::{decode_u16, encode_u16},
        },
    };

    use super::MockEnd;

    const ORDINAL: u64 = 0x0102_0304_0506_0708;
    const TXID: u32 = 0x1112_1314;

    #[test]
    fn write_a_read_b() {
        let (a, b) = MockEnd::pair();
        a.write_message(&[1, 2, 3], &[7]).expect("write ok");
        let mut bytes = [0u8; 8];
        let mut handles = [0u32; 4];
        let len = b.read_message(&mut bytes, &mut handles).expect("read ok");
        assert_eq!(len, MessageLen::new(3, 1));
        assert_eq!(&bytes[..len.bytes], &[1, 2, 3]);
        assert_eq!(&handles[..len.handles], &[7]);
    }

    #[test]
    fn empty_queue_would_block() {
        let (a, _b) = MockEnd::pair();
        let mut bytes = [0u8; 8];
        let mut handles = [0u32; 4];
        assert_eq!(
            a.read_message(&mut bytes, &mut handles),
            Err(IpcError::WouldBlock)
        );
    }

    #[test]
    fn wait_readable_times_out_then_succeeds() {
        let (a, b) = MockEnd::pair();
        assert_eq!(a.wait_readable(0), Err(IpcError::Timeout));
        b.write_message(&[1], &[]).expect("write ok");
        assert_eq!(a.wait_readable(0), Ok(()));
    }

    #[test]
    fn peer_close_drains_then_reports_closed() {
        let (a, b) = MockEnd::pair();
        a.write_message(&[1, 2], &[]).expect("write ok");
        drop(a);
        let mut bytes = [0u8; 8];
        let mut handles = [0u32; 4];
        // Поставленный до закрытия кадр ещё читается.
        let len = b
            .read_message(&mut bytes, &mut handles)
            .expect("drained ok");
        assert_eq!(len.bytes, 2);
        // После опустошения закрытие наблюдается.
        assert_eq!(
            b.read_message(&mut bytes, &mut handles),
            Err(IpcError::PeerClosed)
        );
        assert_eq!(b.wait_readable(0), Err(IpcError::PeerClosed));
    }

    #[test]
    fn write_to_closed_peer_reports_closed() {
        let (a, b) = MockEnd::pair();
        drop(b);
        assert_eq!(a.write_message(&[1], &[]), Err(IpcError::PeerClosed));
    }

    #[test]
    fn read_rejects_buffer_too_small() {
        let (a, b) = MockEnd::pair();
        a.write_message(&[1, 2, 3, 4], &[]).expect("write ok");
        let mut bytes = [0u8; 2];
        let mut handles = [0u32; 4];
        assert_eq!(
            b.read_message(&mut bytes, &mut handles),
            Err(IpcError::Truncated)
        );
        // Кадр не потерян: с достаточным буфером читается.
        let mut big = [0u8; 8];
        let len = b.read_message(&mut big, &mut handles).expect("read ok");
        assert_eq!(len.bytes, 4);
    }

    #[test]
    fn full_frame_round_trip_through_mock() {
        // Сторона A собирает кадр (заголовок + поле), пишет; B читает и разбирает.
        let (a, b) = MockEnd::pair();
        let mut frame = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        frame
            .write_header(&Header::new(ORDINAL, TXID, 0))
            .expect("header ok");
        frame.write_field(1, &encode_u16(0xABCD)).expect("field ok");
        frame.finish().expect("finish ok");
        a.write_message(frame.as_bytes(), &[]).expect("write ok");

        let mut bytes = [0u8; MESSAGE_INLINE_MAX];
        let mut handles = [0u32; 4];
        let len = b.read_message(&mut bytes, &mut handles).expect("read ok");
        assert_eq!(len.handles, 0);

        let received = &bytes[..len.bytes];
        let header = Header::decode(received).expect("header decode");
        assert_eq!(header.ordinal, ORDINAL);
        assert_eq!(header.txid, TXID);

        let mut cursor = FieldCursor::new(&received[HEADER_SIZE..]);
        let field = cursor.next_field().expect("ok").expect("present");
        assert_eq!(field.id, 1);
        assert_eq!(decode_u16(field.data), Ok(0xABCD));
        assert_eq!(cursor.next_field().expect("ok"), None);
    }
}
