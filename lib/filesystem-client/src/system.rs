//! Окружение фасада поверх объектов ядра. Сессия становится транспортом её клиента, а
//! capability региона - отображением данных сессии.

use filesystem_abi::{FileSystemError, IO_FRAME_BYTES, IoRecord, file_system, namespace};
use ipc::{
    Timeout,
    wire::{IpcError, MessageBuf},
};
use kernel_objects::{
    IssuedHandle, Mapping, MemoryRegion, ReceivedSession, RingConsumer, RingProducer,
    SessionTransport, Signal,
};
use syscall::{UserMemFlags, kind};

use crate::{
    FileSystem,
    environment::{Environment, IoContext, SharedRegion},
};

/// Окружение фасада файловой подсистемы поверх объектов ядра.
pub struct FileSystemEnvironment;

/// Отображённый регион данных сессии.
pub struct SessionRegion {
    mapping: Mapping,
    writable: bool,
}

/// Пара колец, уведомлений и scratch одной файловой сессии.
pub struct SessionIo {
    exchange: Option<Exchange>,
    outgoing: MessageBuf,
    incoming: [u8; IO_FRAME_BYTES],
}

struct Exchange {
    requests: RingProducer,
    responses: RingConsumer,
    scratch: SessionRegion,
}

impl FileSystem<FileSystemEnvironment> {
    /// Фасад над маршрутизатором имён. Путь разбирается по монтированиям.
    pub fn new(namespace: kernel_objects::Session<namespace::Protocol>) -> Self {
        Self::with_environment(FileSystemEnvironment, namespace.client().into_channel())
    }
}

impl SharedRegion for SessionRegion {
    fn size(&self) -> usize {
        usize::try_from(self.mapping.size_bytes()).unwrap_or(usize::MAX)
    }

    fn read_at(&self, offset: usize, dst: &mut [u8]) -> Result<(), FileSystemError> {
        self.mapping
            .copy_out_at(offset, dst)
            .map_err(|_| FileSystemError::BufferTooSmall)
    }

    fn write_at(&self, offset: usize, src: &[u8]) -> Result<(), FileSystemError> {
        if !self.writable {
            return Err(FileSystemError::WrongMode);
        }

        self.mapping
            .copy_in_at(offset, src)
            .map_err(|_| FileSystemError::BufferTooSmall)
    }
}

impl Environment for FileSystemEnvironment {
    type Handle<K: syscall::ObjectKind> = IssuedHandle<K>;
    type Transport = SessionTransport;
    type Region = SessionRegion;
    type Io = SessionIo;

    fn io(&self) -> SessionIo {
        SessionIo {
            exchange: None,
            outgoing: MessageBuf::new(),
            incoming: [0; IO_FRAME_BYTES],
        }
    }

    fn connect(
        &self,
        session: ReceivedSession<file_system::Protocol>,
    ) -> Result<SessionTransport, FileSystemError> {
        Ok(kernel_objects::Session::from_session(session)
            .client()
            .into_channel())
    }
}

impl SessionRegion {
    fn map(
        region: IssuedHandle<kind::MemoryRegion>,
        writable: bool,
    ) -> Result<Self, FileSystemError> {
        let flags = if writable {
            UserMemFlags::ReadWrite
        } else {
            UserMemFlags::ReadOnly
        };

        let mapping = Mapping::full(MemoryRegion::from(region), flags)
            .map_err(|_| FileSystemError::NoSpace)?;
        Ok(Self { mapping, writable })
    }
}

fn started<'a>(
    exchange: &'a mut Option<Exchange>,
    client: &file_system::Client<SessionTransport>,
) -> crate::Result<&'a Exchange> {
    if let Some(established) = exchange {
        return Ok(established);
    }

    let ends = client.start_io().wait()?;
    let requests = RingProducer::new(
        MemoryRegion::from(ends.requests),
        IO_FRAME_BYTES,
        Signal::from(ends.request_ready),
    )
    .ok_or(FileSystemError::NoSpace)?;
    let responses = RingConsumer::new(
        MemoryRegion::from(ends.responses),
        IO_FRAME_BYTES,
        Signal::from(ends.response_ready),
    )
    .ok_or(FileSystemError::NoSpace)?;
    let scratch = SessionRegion::map(client.take_io_scratch().wait()?, true)?;
    Ok(exchange.insert(Exchange {
        requests,
        responses,
        scratch,
    }))
}

impl IoContext<FileSystemEnvironment> for SessionIo {
    fn exchange(
        &mut self,
        client: &file_system::Client<SessionTransport>,
        request: IoRecord,
        wait: Timeout,
    ) -> crate::Result<IoRecord> {
        request.encode(&mut self.outgoing)?;
        let exchange = started(&mut self.exchange, client)?;
        exchange.requests.push(self.outgoing.as_bytes())?;
        let len = exchange.responses.pop(&mut self.incoming, wait)?;
        Ok(IoRecord::decode(
            self.incoming.get(..len).ok_or(IpcError::Truncated)?,
        )?)
    }

    fn scratch(
        &mut self,
        client: &file_system::Client<SessionTransport>,
    ) -> crate::Result<&SessionRegion> {
        Ok(&started(&mut self.exchange, client)?.scratch)
    }

    fn map(
        &self,
        region: IssuedHandle<kind::MemoryRegion>,
        writable: bool,
    ) -> Result<SessionRegion, FileSystemError> {
        SessionRegion::map(region, writable)
    }
}
