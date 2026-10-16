//! Распределитель дескрипторов: список свободных и длины выданных цепочек по их головам.
//!
//! Очередь получает цепочку через [`Descriptors::reserve`], который состояние не меняет,
//! и закрепляет её через [`Descriptors::commit`] после публикации устройству.

use super::{QUEUE_SIZE_MAX, QueueError};

const DESCRIPTORS_CAPACITY: usize = QUEUE_SIZE_MAX as usize;

pub(super) struct Descriptors {
    next: [u16; DESCRIPTORS_CAPACITY],
    chain_lengths: [u16; DESCRIPTORS_CAPACITY],
    free_head: u16,
    free_count: u16,
}

pub(super) struct Chain {
    pub(super) head: u16,
    pub(super) tail: u16,
    next_free: u16,
    length: u16,
}

impl Descriptors {
    pub(super) fn new(size: u16) -> Self {
        let mut next = [0; DESCRIPTORS_CAPACITY];
        for (position, link) in (1..).zip(next.iter_mut()) {
            *link = position;
        }
        Self {
            next,
            chain_lengths: [0; DESCRIPTORS_CAPACITY],
            free_head: 0,
            free_count: size,
        }
    }

    pub(super) fn reserve<E>(&self, length: u16) -> Result<Chain, QueueError<E>> {
        if length == 0 || self.free_count < length {
            return Err(QueueError::Full);
        }
        let head = self.free_head;
        let tail = self.walk(head, length - 1)?;
        let next_free = self.link(tail)?;
        Ok(Chain {
            head,
            tail,
            next_free,
            length,
        })
    }

    pub(super) fn commit(
        &mut self,
        Chain {
            head,
            next_free,
            length,
            ..
        }: Chain,
    ) {
        if let Some(chain_length) = self.chain_lengths.get_mut(usize::from(head)) {
            *chain_length = length;
            self.free_head = next_free;
            self.free_count -= length;
        }
    }

    pub(super) fn release<E>(&mut self, id: u32) -> Result<u16, QueueError<E>> {
        let head = u16::try_from(id).map_err(|_| QueueError::UnknownHead(id))?;
        let length = self
            .chain_lengths
            .get(usize::from(head))
            .copied()
            .filter(|length| *length != 0)
            .ok_or(QueueError::UnknownHead(id))?;
        let tail = self.walk(head, length - 1)?;
        let tail_link = self
            .next
            .get_mut(usize::from(tail))
            .ok_or(QueueError::UnknownHead(id))?;

        *tail_link = self.free_head;
        if let Some(chain_length) = self.chain_lengths.get_mut(usize::from(head)) {
            *chain_length = 0;
        }
        self.free_head = head;
        self.free_count += length;
        Ok(head)
    }

    fn walk<E>(&self, from: u16, links: u16) -> Result<u16, QueueError<E>> {
        let mut descriptor = from;
        for _ in 0..links {
            descriptor = self.link(descriptor)?;
        }
        Ok(descriptor)
    }

    pub(super) fn link<E>(&self, descriptor: u16) -> Result<u16, QueueError<E>> {
        let next = self.next.get(usize::from(descriptor)).copied();
        next.ok_or(QueueError::UnknownHead(descriptor.into()))
    }
}
