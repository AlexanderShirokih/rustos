//! Обход каталога. Записи приходят порциями в буфер данных сессии.

use alloc::vec::Vec;

use filesystem_abi::{DirCursor, DirEntry, FileSystemError, NodeDescriptor};

use crate::{
    environment::{Environment, SharedRegion},
    error::Result,
    node::OpenNode,
    session::Session,
};

/// Обход каталога записями в порядке перечисления. Каталог и его сессия принадлежат обходу.
pub struct Listing<E: Environment> {
    node: OpenNode<E>,
    walk: Walk,
}

pub(crate) struct Walk {
    chunk: Vec<u8>,
    offset: usize,
    remaining: u32,
    cursor: DirCursor,
    end: bool,
}

impl<E: Environment> Listing<E> {
    pub(crate) fn new(node: OpenNode<E>) -> Result<Self> {
        let walk = Walk::new(node.session().capacity())?;
        Ok(Self { node, walk })
    }

    /// Следующая запись каталога. `None` - обход закончен. Запись, которая длиннее буфера данных
    /// сессии, останавливает обход ответом `BufferTooSmall`.
    pub fn next_entry(&mut self) -> Result<Option<DirEntry<'_>>> {
        let Self { node, walk } = self;
        walk.next_entry(node.session(), node.descriptor())
    }
}

impl Walk {
    pub(crate) fn new(capacity: usize) -> Result<Self> {
        let mut chunk = Vec::new();
        chunk
            .try_reserve_exact(capacity)
            .map_err(|_| FileSystemError::NoSpace)?;
        chunk.resize(capacity, 0);

        Ok(Self {
            chunk,
            offset: 0,
            remaining: 0,
            cursor: DirCursor::START,
            end: false,
        })
    }

    pub(crate) fn next_entry<E: Environment>(
        &mut self,
        session: &Session<E>,
        descriptor: NodeDescriptor,
    ) -> Result<Option<DirEntry<'_>>> {
        while self.remaining == 0 {
            if self.end {
                return Ok(None);
            }
            self.fill(session, descriptor)?;
        }
        let start = self.offset;
        let tail = self
            .chunk
            .get(start..)
            .ok_or(FileSystemError::BufferTooSmall)?;
        let decoded = DirEntry::decode(tail)?;

        self.offset = start.saturating_add(decoded.len);
        self.remaining = self.remaining.saturating_sub(1);
        Ok(Some(decoded.entry))
    }

    fn fill<E: Environment>(
        &mut self,
        session: &Session<E>,
        descriptor: NodeDescriptor,
    ) -> Result<()> {
        let listed = session.client().list(descriptor, self.cursor)?.value?;
        if listed.count == 0 && !listed.end {
            return Err(FileSystemError::BufferTooSmall.into());
        }
        session.region().read_at(0, &mut self.chunk)?;

        self.cursor = listed.next;
        self.end = listed.end;
        self.remaining = listed.count;
        self.offset = 0;
        Ok(())
    }
}
