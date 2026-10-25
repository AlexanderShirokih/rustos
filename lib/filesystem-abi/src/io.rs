//! Записи единственного файлового обмена в разделяемых кольцах. Capability передаются
//! управляющим портом до подтверждения регистрации. Слот кольца содержит только описание.
//!
//! Перенос через scratch сессии занимает один обмен. Клиент кладёт байты записи в scratch до
//! запроса, сервер кладёт байты чтения и перечисления в scratch до Done. Перенос через окно
//! проходит Register, Ready и Finish.

use ipc::{
    IntoWireValue, WireValue,
    wire::{FIELD_ID_MIN, FieldCursor, IpcError, MessageBuf},
};

use crate::{DirCursor, FileSystemError, Listed, NodeDescriptor};

/// Предел полезных данных одной операции, независимый от размера слота кольца.
pub const IO_TRANSFER_MAX: u32 = 64 * 1024;

/// Ёмкость слота для управляющей записи, закодированной общим IPC-кодеком.
pub const IO_FRAME_BYTES: usize = 256;

/// Размер scratch сессии и предел операции, перенос которой идёт через него.
pub const IO_SCRATCH_BYTES: u32 = 16 * 1024;

/// Чтение или запись `len` байтов переносится через scratch сессии.
#[must_use]
pub const fn fits_scratch(len: usize) -> bool {
    len <= IO_SCRATCH_BYTES as usize
}

/// Ёмкость пачки перечисления для буфера в `len` байтов, вмещающаяся в scratch сессии.
#[must_use]
pub const fn scratch_room(len: usize) -> usize {
    if fits_scratch(len) {
        len
    } else {
        IO_SCRATCH_BYTES as usize
    }
}

ipc::wire_enum! {
    /// Фаза файловой операции одной сессии. До Done следующая операция не начинается.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum IoPhase {
        /// Запрос диапазона для чтения.
        Read = 1,
        /// Запрос диапазона для записи, включая APPEND_OFFSET.
        Write = 2,
        /// Запрос пачки записей каталога. Записи переносятся через scratch.
        List = 3,
        /// Получить capability поколения через take_io_region и отобразить её.
        Register = 4,
        /// Capability отображена, клиент подтверждает её поколение.
        Registered = 5,
        /// Диапазон доступен для переноса клиентом.
        Ready = 6,
        /// Перенос завершён; size задаёт фактически перенесённый префикс.
        Finish = 7,
        /// Клиент отказывается от операции.
        Abort = 8,
        /// Операция завершена, result содержит число байтов либо отказ.
        Done = 9,
    }
}

ipc::wire_value! {
    /// Описание одной операции: id не повторяется в сессии, region обозначает поколение
    /// capability. offset отсчитывается в файле, start - в выданном регионе. Чужое поколение,
    /// фаза или число байтов за границей запроса отвергаются до обращения к данным.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct IoRecord {
        /// Последовательный номер операции клиента.
        pub id: u64,
        /// Фаза обмена.
        pub phase: IoPhase,
        /// Значение дескриптора внутри сессии.
        pub fd: u32,
        /// Смещение файла либо позиция перечисления для List.
        pub offset: u64,
        /// Длина запроса либо перенесённого префикса.
        pub size: u32,
        /// Ненулевое поколение зарегистрированного региона.
        pub region: u64,
        /// Начало диапазона в регионе.
        pub start: u64,
        /// Окно допускает запись; RW-дескриптор сохраняет такое отображение между операциями.
        pub writable: bool,
        /// Read или Write переносит байты через scratch сессии со смещения 0.
        pub scratch: bool,
        /// Число доступных/завершённых байтов либо отказ.
        pub result: Result<u32, FileSystemError>,
        /// Результат перечисления; другие операции его не используют.
        pub listed: Listed,
    }
}

ipc::wire_value! {
    /// Клиентские концы пары колец. Сервер удерживает их корневые capability до ухода сессии.
    pub struct IoBootstrap<
        M = ipc::wire::Cap<syscall::kind::MemoryRegion>,
        S = ipc::wire::Cap<syscall::kind::Signal>,
    > {
        /// Кольцо запросов, у клиента единственный producer.
        pub requests: M,
        /// Уведомление потребителя запросов.
        pub request_ready: S,
        /// Кольцо ответов, у клиента единственный consumer.
        pub responses: M,
        /// Уведомление потребителя ответов.
        pub response_ready: S,
    }
}

impl IoRecord {
    /// Запрос фазы `phase` к дескриптору `fd` без поколения региона и результата.
    #[must_use]
    pub const fn request(
        id: u64,
        phase: IoPhase,
        fd: NodeDescriptor,
        offset: u64,
        size: u32,
    ) -> Self {
        Self {
            id,
            phase,
            fd: fd.raw(),
            offset,
            size,
            region: 0,
            start: 0,
            writable: false,
            scratch: false,
            result: Ok(0),
            listed: Listed {
                count: 0,
                next: DirCursor::START,
                end: true,
            },
        }
    }

    /// Кодирует описание в переиспользуемый буфер без capability.
    pub fn encode(self, message: &mut MessageBuf) -> Result<(), IpcError> {
        drop(message.outgoing());
        self.write_into(message, FIELD_ID_MIN)?;
        message.finish()?;
        if message.len() > IO_FRAME_BYTES {
            return Err(IpcError::FrameOverflow);
        }

        Ok(())
    }

    /// Читает описание. Дополнительные внешние поля и усечённые значения дают ошибку.
    pub fn decode(bytes: &[u8]) -> Result<Self, IpcError> {
        let mut fields = FieldCursor::new(bytes);
        let field = fields.next_field()?.ok_or(IpcError::MissingField)?;
        if field.id != FIELD_ID_MIN || fields.next_field()?.is_some() {
            return Err(IpcError::InvalidValue);
        }
        Self::from_field(field.data, &mut ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_reuse_the_codec_buffer_without_a_tail_from_the_previous_reply() {
        let mut encoded = MessageBuf::new();
        for result in [Ok(u32::MAX), Err(FileSystemError::NoSpace), Ok(0)] {
            let record = IoRecord {
                id: u64::MAX,
                phase: IoPhase::Done,
                fd: u32::MAX,
                offset: u64::MAX,
                size: u32::MAX,
                region: u64::MAX,
                start: u64::MAX,
                writable: true,
                scratch: true,
                result,
                listed: Listed {
                    count: u32::MAX,
                    next: DirCursor::from_raw(u32::MAX),
                    end: true,
                },
            };

            record.encode(&mut encoded).expect("encode");
            assert_eq!(IoRecord::decode(encoded.as_bytes()), Ok(record));
            assert!(IoRecord::decode(&encoded.as_bytes()[..encoded.len() - 3]).is_err());
        }
    }
}
