use alloc::vec::Vec;
use core::num::NonZeroU32;

use super::{arch::ArchContext, thread::Thread};
use crate::{SpawnError, ThreadId};

pub struct ThreadTable<A: ArchContext> {
    slots: Vec<Option<Thread<A>>>,
    max_threads: usize,
}

impl<A: ArchContext> ThreadTable<A> {
    pub fn new(max_threads: usize) -> Self {
        Self {
            // Ёмкость берётся сразу и не меняется: планировщик передаёт в
            // переключение контекста указатели на потоки, а другой CPU в это
            // время может занять слот. Рост таблицы сдвинул бы потоки и
            // оставил эти указатели висеть. Слотов не больше `max_threads`,
            // поэтому дальше `push` не перевыделяет.
            slots: Vec::with_capacity(max_threads),
            max_threads,
        }
    }

    /// Резервирует свободный слот и заполняет его потоком, сконструированным
    /// замыканием `builder` с использованием выделенного `ThreadId`.
    ///
    /// Возвращает сам вставленный поток. Его id доступен через [`Thread::id`].
    pub fn insert_with<F>(&mut self, builder: F) -> Result<&mut Thread<A>, SpawnError>
    where
        F: FnOnce(ThreadId) -> Thread<A>,
    {
        let index = if let Some(index) = self.slots.iter().position(Option::is_none) {
            index
        } else {
            if self.slots.len() >= self.max_threads {
                return Err(SpawnError::NoFreeThreadSlots);
            }
            self.slots.push(None);
            self.slots.len() - 1
        };

        let id = Self::slot_id(index);
        let thread = builder(id);
        debug_assert_eq!(thread.id(), id, "builder must store assigned ThreadId");
        let slot = self
            .slots
            .get_mut(index)
            .ok_or(SpawnError::NoFreeThreadSlots)?;
        Ok(slot.insert(thread))
    }

    pub fn get(&self, id: ThreadId) -> Option<&Thread<A>> {
        self.slots.get(Self::slot_index(id))?.as_ref()
    }

    pub fn get_mut(&mut self, id: ThreadId) -> Option<&mut Thread<A>> {
        self.slots.get_mut(Self::slot_index(id))?.as_mut()
    }

    pub fn remove(&mut self, id: ThreadId) -> Option<Thread<A>> {
        self.slots.get_mut(Self::slot_index(id))?.take()
    }

    /// Линейный обход живых потоков.
    pub fn iter(&self) -> impl Iterator<Item = &Thread<A>> {
        self.slots.iter().filter_map(|s| s.as_ref())
    }

    pub fn split_pair_mut(
        &mut self,
        first: ThreadId,
        second: ThreadId,
    ) -> Option<(&mut Thread<A>, &mut Thread<A>)> {
        let first_index = Self::slot_index(first);
        let second_index = Self::slot_index(second);

        if first_index == second_index {
            return None;
        }

        if first_index < second_index {
            let (head, tail) = self.slots.split_at_mut(second_index);
            let first_ref = head.get_mut(first_index)?.as_mut()?;
            let second_ref = tail.first_mut()?.as_mut()?;
            Some((first_ref, second_ref))
        } else {
            let (head, tail) = self.slots.split_at_mut(first_index);
            let second_ref = head.get_mut(second_index)?.as_mut()?;
            let first_ref = tail.first_mut()?.as_mut()?;
            Some((first_ref, second_ref))
        }
    }

    fn slot_index(id: ThreadId) -> usize {
        id.raw().get() as usize - 1
    }

    fn slot_id(index: usize) -> ThreadId {
        ThreadId::new(NonZeroU32::MIN.saturating_add(index as u32))
    }
}

impl<A: ArchContext> Default for ThreadTable<A> {
    fn default() -> Self {
        Self::new(64)
    }
}
