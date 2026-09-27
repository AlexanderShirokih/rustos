//! Открытый узел файловой системы: его сессия, дескриптор, позиция и операции над
//! содержимым.

use filesystem_abi::{
    APPEND_OFFSET, FileSystemError, NodeDescriptor, NodeInfo, NodeKind, Opened, Written,
};
use syscall::kind;

use crate::{
    environment::{Environment, SharedRegion},
    error::Result,
    listing::Listing,
    session::Session,
};

/// Узел, открытый в собственной сессии. Дескриптор и сессия закрываются вместе с объектом.
pub struct OpenNode<E: Environment> {
    session: Session<E>,
    descriptor: NodeDescriptor,
    kind: NodeKind,
    size: u64,
    position: u64,
}

impl<E: Environment> OpenNode<E> {
    pub(crate) fn new(session: Session<E>, opened: Opened) -> Self {
        Self {
            session,
            descriptor: opened.fd,
            kind: opened.kind,
            size: opened.size,
            position: 0,
        }
    }

    /// Вид узла на момент открытия.
    pub fn kind(&self) -> NodeKind {
        self.kind
    }

    /// Размер узла на момент открытия.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Позиция чтения-записи.
    pub fn position(&self) -> u64 {
        self.position
    }

    /// Задаёт позицию чтения-записи.
    pub fn seek(&mut self, position: u64) {
        self.position = position;
    }

    /// Читает узел с текущей позиции и продвигает её на число прочитанных байт. Возвращает
    /// число прочитанных байт, которое может быть меньше, чем вмещает `dst`, либо ошибку,
    /// если не удаётся прочитать ни одного байта.
    pub fn read(&mut self, dst: &mut [u8]) -> Result<usize> {
        let mut done = 0;
        for chunk in dst.chunks_mut(self.session.capacity().max(1)) {
            let count = match self.read_chunk(chunk) {
                Ok(0) => break,
                Ok(count) => count,
                Err(_) if done > 0 => break,
                Err(error) => return Err(error),
            };
            self.advance(count);
            done += count;
            if count < chunk.len() {
                break;
            }
        }
        Ok(done)
    }

    /// Пишет в узел с текущей позиции и продвигает её на число записанных байт. Возвращает
    /// число записанных байт, которое может быть меньше, чем содержит `src`, либо ошибку,
    /// если не удаётся записать ни одного байта.
    pub fn write(&mut self, src: &[u8]) -> Result<usize> {
        let mut done = 0;
        for chunk in src.chunks(self.session.capacity().max(1)) {
            let count = match self.write_chunk(chunk) {
                Ok(0) => break,
                Ok(count) => count,
                Err(_) if done > 0 => break,
                Err(error) => return Err(error),
            };
            self.advance(count);
            done += count;
            if count < chunk.len() {
                break;
            }
        }
        Ok(done)
    }

    fn read_chunk(&self, chunk: &mut [u8]) -> Result<usize> {
        let requested = self.session.wire_len(chunk.len());
        let count = self
            .session
            .client()
            .read(self.descriptor, self.position, requested)??;
        let count = usize::try_from(count).unwrap_or(0).min(chunk.len());
        if let Some(head) = chunk.get_mut(..count) {
            self.session.region().read_at(0, head)?;
        }
        Ok(count)
    }

    fn write_chunk(&self, chunk: &[u8]) -> Result<usize> {
        self.session.region().write_at(0, chunk)?;
        let requested = self.session.wire_len(chunk.len());
        let written = self
            .session
            .client()
            .write(self.descriptor, self.position, requested)??;
        Ok(usize::try_from(written.count).unwrap_or(0).min(chunk.len()))
    }

    fn advance(&mut self, count: usize) {
        self.position = self
            .position
            .saturating_add(u64::try_from(count).unwrap_or(0));
    }

    /// Дозаписывает байты в конец узла независимо от текущей позиции одной файловой
    /// операцией. Срез длиннее буфера данных сессии дозаписывается помещающимся префиксом.
    /// Исход содержит длину этого префикса, а остаток дозаписывает следующий вызов. Позиция
    /// чтения-записи остаётся прежней.
    pub fn append(&self, src: &[u8]) -> Result<Written> {
        let head = src
            .get(..src.len().min(self.session.capacity()))
            .ok_or(FileSystemError::BufferTooSmall)?;
        self.session.region().write_at(0, head)?;

        let len = self.session.wire_len(head.len());
        Ok(self
            .session
            .client()
            .write(self.descriptor, APPEND_OFFSET, len)??)
    }

    /// Задаёт длину узла. Байты за `size` отбрасываются, расширение дописывает нули. Позиция
    /// чтения-записи остаётся прежней.
    pub fn set_size(&self, size: u64) -> Result<()> {
        self.session.client().set_size(self.descriptor, size)??;
        Ok(())
    }

    /// Метаданные узла на сервере сейчас. Значения [`kind`](Self::kind) и [`size`](Self::size)
    /// остаются от открытия.
    pub fn metadata(&self) -> Result<NodeInfo> {
        Ok(self.session.client().stat(self.descriptor)??)
    }

    /// Capability региона, на котором лежит содержимое узла. Права выводятся из режима открытия.
    /// После выдачи длина узла за ёмкость региона не растёт. Пустой узел и файловая система без
    /// такого региона возвращают `NotSupported`.
    pub fn backing_memory(&self) -> Result<E::Handle<kind::MemoryRegion>> {
        Ok(self.session.client().backing_memory(self.descriptor)??)
    }

    /// Обход каталога с начала перечисления.
    pub fn entries(self) -> Result<Listing<E>> {
        Listing::new(self)
    }

    pub(crate) fn session(&self) -> &Session<E> {
        &self.session
    }

    pub(crate) fn descriptor(&self) -> NodeDescriptor {
        self.descriptor
    }
}

impl<E: Environment> Drop for OpenNode<E> {
    fn drop(&mut self) {
        let _ = self.session.client().close(self.descriptor);
    }
}
