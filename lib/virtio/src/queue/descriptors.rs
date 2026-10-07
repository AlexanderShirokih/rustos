//! Распределитель дескрипторов: список свободных и выданные головы цепочек.
//!
//! Очередь получает цепочку через [`Descriptors::reserve`], который состояние не меняет,
//! и закрепляет её через [`Descriptors::commit`] после публикации устройству.

use super::{CHAIN_DESCRIPTORS, QUEUE_SIZE_MAX, QueueError};

const DESCRIPTORS_CAPACITY: usize = QUEUE_SIZE_MAX as usize;

pub(super) struct Descriptors {
    next: [u16; DESCRIPTORS_CAPACITY],
    issued: [bool; DESCRIPTORS_CAPACITY],
    free_head: u16,
    free_count: u16,
}

pub(super) struct Chain {
    pub(super) head: u16,
    pub(super) tail: u16,
    next_free: u16,
}

impl Descriptors {
    pub(super) fn new(size: u16) -> Self {
        let mut next = [0; DESCRIPTORS_CAPACITY];
        for (position, link) in (1..).zip(next.iter_mut()) {
            *link = position;
        }
        Self {
            next,
            issued: [false; DESCRIPTORS_CAPACITY],
            free_head: 0,
            free_count: size,
        }
    }

    pub(super) fn reserve<E>(&self) -> Result<Chain, QueueError<E>> {
        if self.free_count < CHAIN_DESCRIPTORS {
            return Err(QueueError::Full);
        }
        let head = self.free_head;
        let tail = self.link(head)?;
        let next_free = self.link(tail)?;
        Ok(Chain {
            head,
            tail,
            next_free,
        })
    }

    pub(super) fn commit(
        &mut self,
        Chain {
            head, next_free, ..
        }: Chain,
    ) {
        if let Some(issued) = self.issued.get_mut(usize::from(head)) {
            *issued = true;
            self.free_head = next_free;
            self.free_count -= CHAIN_DESCRIPTORS;
        }
    }

    pub(super) fn release<E>(&mut self, id: u32) -> Result<u16, QueueError<E>> {
        let head = u16::try_from(id).map_err(|_| QueueError::UnknownHead(id))?;
        let tail = self.link(head)?;
        let issued = self
            .issued
            .get_mut(usize::from(head))
            .filter(|issued| **issued);
        let tail_link = self.next.get_mut(usize::from(tail));
        let (Some(issued), Some(tail_link)) = (issued, tail_link) else {
            return Err(QueueError::UnknownHead(id));
        };

        *issued = false;
        *tail_link = self.free_head;
        self.free_head = head;
        self.free_count += CHAIN_DESCRIPTORS;
        Ok(head)
    }

    fn link<E>(&self, descriptor: u16) -> Result<u16, QueueError<E>> {
        let next = self.next.get(usize::from(descriptor)).copied();
        next.ok_or(QueueError::UnknownHead(descriptor.into()))
    }
}
