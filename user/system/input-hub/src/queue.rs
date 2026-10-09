//! Очередь записей сессии `Input`.

use alloc::collections::VecDeque;

use input_abi::{Kind, MESSAGE_RECORDS_MAX, RECORD_SIZE, Record, RecordBuf};

pub(crate) const QUEUE_RECORDS_MAX: usize = 4096;

type Raw = [u8; RECORD_SIZE];

pub(crate) struct Queue {
    records: VecDeque<Raw>,
}

impl Queue {
    pub(crate) fn new() -> Option<Self> {
        let mut records = VecDeque::new();
        records.try_reserve_exact(QUEUE_RECORDS_MAX).ok()?;
        Some(Self { records })
    }

    pub(crate) fn fits(&self, count: usize) -> bool {
        self.records.len() + count <= QUEUE_RECORDS_MAX
    }

    pub(crate) fn push(&mut self, records: &[Raw]) {
        self.records.extend(records.iter().copied());
    }

    pub(crate) fn clear(&mut self) {
        self.records.clear();
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub(crate) fn take_message(&mut self) -> RecordBuf {
        let mut message = RecordBuf::new();
        loop {
            let unit = self.unit_len();
            if unit == 0 || message.len() + unit > MESSAGE_RECORDS_MAX {
                return message;
            }

            for raw in self.records.drain(..unit) {
                // Кадр не длиннее сообщения, и проверка выше держит записи в его пределах.
                let _ = message.push_bytes(&raw);
            }
        }
    }

    fn unit_len(&self) -> usize {
        for (index, raw) in self.records.iter().enumerate() {
            match Record::from_bytes(raw).kind {
                Kind::Frame => return index + 1,
                Kind::Resync | Kind::DeviceAdded | Kind::DeviceRemoved if index == 0 => return 1,
                _ => {}
            }
        }
        self.records.len()
    }
}
