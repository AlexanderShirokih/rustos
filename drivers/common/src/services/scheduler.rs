//! Контракты подсистемы планировщика.

use alloc::boxed::Box;
use core::num::NonZeroU32;

use crate::services::Service;

/// Идентификатор потока.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct ThreadId(NonZeroU32);

impl ThreadId {
    pub const fn new(raw: NonZeroU32) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> NonZeroU32 {
        self.0
    }
}

/// Приоритет потока: `0` - наивысший, большее число - меньший приоритет.
///
/// Допустимый верхний предел зависит от количества уровней приоритета,
/// сконфигурированных в конкретном экземпляре `Scheduler`. Поток с
/// `priority.raw() >= PRIO` будет отвергнут планировщиком как
/// `SpawnError::InvalidPriority`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Priority(u8);

impl Priority {
    /// Высший возможный приоритет.
    pub const HIGHEST: Self = Self(0);

    /// Условный "средний" приоритет, удобный для пользовательских процессов.
    pub const NORMAL: Self = Self(15);

    pub const fn new(raw: u8) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u8 {
        self.0
    }

    pub const fn normal() -> Self {
        Self::NORMAL
    }

    pub const fn highest() -> Self {
        Self::HIGHEST
    }
}

/// Конфигурация нового потока.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnConfig {
    pub name: &'static str,
    pub priority: Priority,
    pub stack_pages: usize,
}

impl SpawnConfig {
    pub const DEFAULT_STACK_PAGES: usize = 4;

    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            priority: Priority::NORMAL,
            stack_pages: Self::DEFAULT_STACK_PAGES,
        }
    }

    pub const fn priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    pub const fn stack_pages(mut self, stack_pages: usize) -> Self {
        self.stack_pages = stack_pages;
        self
    }
}

/// Ошибки запуска и управления потоком.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnError {
    NoFreeThreadSlots,
    InvalidPriority,
    InvalidStackPages,
    StackAllocationFailed,
}

/// Контракт сервиса планировщика.
pub trait SchedulerService: Service {
    fn spawn_boxed(
        &self,
        cfg: SpawnConfig,
        entry: Box<dyn FnOnce() + Send + 'static>,
    ) -> Result<ThreadId, SpawnError>;

    fn yield_now(&self);

    fn sleep_ns(&self, ns: u64);

    fn current(&self) -> ThreadId;

    fn exit(&self) -> !;
}

/// Generic-обёртки над boxed service API.
pub trait SchedulerServiceExt: SchedulerService {
    fn spawn<F: FnOnce() + Send + 'static>(
        &self,
        cfg: SpawnConfig,
        entry: F,
    ) -> Result<ThreadId, SpawnError> {
        self.spawn_boxed(cfg, Box::new(entry))
    }

    fn sleep_ms(&self, ms: u64) {
        self.sleep_ns(ms.saturating_mul(1_000_000));
    }
}

impl<T: SchedulerService + ?Sized> SchedulerServiceExt for T {}
