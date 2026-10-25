//! Открытый узел файловой системы: его сессия, дескриптор, позиция и операции над
//! содержимым.

use filesystem_abi::{
    APPEND_OFFSET, FileSystemError, IO_TRANSFER_MAX, NodeDescriptor, NodeInfo, NodeKind, Opened,
    Written,
};

use crate::{environment::Environment, error::Result, listing::Listing, session::Session};

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
            kind: opened.info.kind,
            size: opened.info.size,
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
        for chunk in dst.chunks_mut(IO_TRANSFER_MAX as usize) {
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
        for chunk in src.chunks(IO_TRANSFER_MAX as usize) {
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
        self.session.read(self.descriptor, self.position, chunk)
    }

    fn write_chunk(&self, chunk: &[u8]) -> Result<usize> {
        Ok(usize::try_from(
            self.session
                .write(self.descriptor, self.position, chunk)?
                .count,
        )
        .unwrap_or(0))
    }

    fn advance(&mut self, count: usize) {
        self.position = self
            .position
            .saturating_add(u64::try_from(count).unwrap_or(0));
    }

    /// Дозаписывает байты в конец узла независимо от текущей позиции одной файловой
    /// операцией. Срез длиннее предела одной операции дозаписывается допустимым префиксом.
    /// Исход содержит длину этого префикса, а остаток дозаписывает следующий вызов. Позиция
    /// чтения-записи остаётся прежней.
    pub fn append(&self, src: &[u8]) -> Result<Written> {
        let head = src
            .get(..src.len().min(IO_TRANSFER_MAX as usize))
            .ok_or(FileSystemError::BufferTooSmall)?;
        self.session.write(self.descriptor, APPEND_OFFSET, head)
    }

    /// Задаёт длину узла. Байты за `size` отбрасываются, расширение дописывает нули. Позиция
    /// чтения-записи остаётся прежней.
    pub fn set_size(&self, size: u64) -> Result<()> {
        self.session
            .client()
            .set_size(self.descriptor, size)
            .wait()?;
        Ok(())
    }

    /// Метаданные узла на сервере сейчас. Значения [`kind`](Self::kind) и [`size`](Self::size)
    /// остаются от открытия.
    pub fn metadata(&self) -> Result<NodeInfo> {
        Ok(self.session.client().stat(self.descriptor).wait()?)
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
        let _ = self.session.client().close(self.descriptor).wait();
    }
}
