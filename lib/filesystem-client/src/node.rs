//! Открытый узел файловой системы: его дескриптор, позиция и операции над содержимым.

use filesystem_abi::{
    APPEND_OFFSET, DirCursor, FileSystemError, Listed, NodeDescriptor, NodeInfo, NodeKind, Opened,
    Written,
};
use ipc::wire::Cap;

use crate::{
    environment::{Environment, SharedRegion},
    error::Result,
    listing::Listing,
    session::Session,
};

/// Узел, открытый в сессии. Дескриптор закрывается вместе с объектом.
pub struct OpenNode<'a, E: Environment> {
    session: &'a Session<E>,
    descriptor: NodeDescriptor,
    kind: NodeKind,
    size: u64,
    position: u64,
}

impl<'a, E: Environment> OpenNode<'a, E> {
    pub(crate) fn new(session: &'a Session<E>, opened: Opened) -> Self {
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

    /// Читает узел с текущей позиции и продвигает её на число прочитанных байт. Прочитанного
    /// бывает меньше, чем вмещает `dst`.
    pub fn read(&mut self, dst: &mut [u8]) -> Result<usize> {
        let mut done = 0;
        for chunk in dst.chunks_mut(self.session.capacity().max(1)) {
            let requested = self.session.wire_len(chunk.len());
            let count = self
                .session
                .client()
                .read(self.descriptor, self.position, requested)??;
            let count = usize::try_from(count).unwrap_or(0).min(chunk.len());
            if count == 0 {
                break;
            }
            let Some(head) = chunk.get_mut(..count) else {
                break;
            };
            self.session.region().read_at(0, head)?;

            self.position = self
                .position
                .saturating_add(u64::try_from(count).unwrap_or(0));
            done += count;
            if count < chunk.len() {
                break;
            }
        }
        Ok(done)
    }

    /// Пишет в узел с текущей позиции и продвигает её на число записанных байт. Записанного
    /// бывает меньше, чем несёт `src`.
    pub fn write(&mut self, src: &[u8]) -> Result<usize> {
        let mut done = 0;
        for chunk in src.chunks(self.session.capacity().max(1)) {
            self.session.region().write_at(0, chunk)?;
            let requested = self.session.wire_len(chunk.len());
            let written =
                self.session
                    .client()
                    .write(self.descriptor, self.position, requested)??;
            let count = usize::try_from(written.count).unwrap_or(0).min(chunk.len());
            if count == 0 {
                break;
            }
            self.position = self
                .position
                .saturating_add(u64::try_from(count).unwrap_or(0));
            done += count;
            if count < chunk.len() {
                break;
            }
        }
        Ok(done)
    }

    /// Дозаписывает байты в конец узла независимо от текущей позиции. Длина запроса
    /// ограничена буфером данных сессии.
    pub fn append(&self, src: &[u8]) -> Result<Written> {
        if src.len() > self.session.capacity() {
            return Err(FileSystemError::BufferTooSmall.into());
        }
        self.session.region().write_at(0, src)?;

        let len = u32::try_from(src.len()).unwrap_or(u32::MAX);
        Ok(self
            .session
            .client()
            .write(self.descriptor, APPEND_OFFSET, len)??)
    }

    /// Метаданные узла.
    pub fn metadata(&self) -> Result<NodeInfo> {
        Ok(self.session.client().stat(self.descriptor)??)
    }

    /// Capability региона, на котором лежит содержимое узла.
    pub fn backing_memory(&self) -> Result<Cap> {
        Ok(self.session.client().backing_memory(self.descriptor)??)
    }

    /// Обход каталога с начала перечисления.
    pub fn entries(self) -> Result<Listing<'a, E>> {
        Listing::new(self)
    }

    pub(crate) fn session(&self) -> &'a Session<E> {
        self.session
    }

    pub(crate) fn list(&self, from: DirCursor) -> Result<Listed> {
        Ok(self.session.client().list(self.descriptor, from)??)
    }
}

impl<E: Environment> Drop for OpenNode<'_, E> {
    fn drop(&mut self) {
        let _ = self.session.client().close(self.descriptor);
    }
}
