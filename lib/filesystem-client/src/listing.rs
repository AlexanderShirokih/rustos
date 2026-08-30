//! Обход каталога. Записи приходят порциями в буфер данных сессии.

use alloc::vec::Vec;

use filesystem_abi::{DirCursor, DirEntry, FileSystemError};

use crate::{
    environment::{Environment, SharedRegion},
    error::Result,
    node::OpenNode,
};

/// Перечисление каталога, продолжающее обход позицией, которую вернул сервер.
pub struct Listing<'a, E: Environment> {
    node: OpenNode<'a, E>,
    chunk: Vec<u8>,
    offset: usize,
    remaining: u32,
    cursor: DirCursor,
    end: bool,
}

impl<'a, E: Environment> Listing<'a, E> {
    pub(crate) fn new(node: OpenNode<'a, E>) -> Result<Self> {
        let capacity = node.session().capacity();
        let mut chunk = Vec::new();
        chunk
            .try_reserve_exact(capacity)
            .map_err(|_| FileSystemError::NoSpace)?;
        chunk.resize(capacity, 0);

        Ok(Self {
            node,
            chunk,
            offset: 0,
            remaining: 0,
            cursor: DirCursor::START,
            end: false,
        })
    }

    /// Следующая запись каталога. `None` - сервер объявил перечисление законченным.
    pub fn next_entry(&mut self) -> Result<Option<DirEntry<'_>>> {
        while self.remaining == 0 {
            if self.end {
                return Ok(None);
            }
            self.fill()?;
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

    fn fill(&mut self) -> Result<()> {
        let listed = self.node.list(self.cursor)?;
        if listed.count == 0 && !listed.end {
            return Err(FileSystemError::BufferTooSmall.into());
        }
        self.node.session().region().read_at(0, &mut self.chunk)?;

        self.cursor = listed.next;
        self.end = listed.end;
        self.remaining = listed.count;
        self.offset = 0;
        Ok(())
    }
}
