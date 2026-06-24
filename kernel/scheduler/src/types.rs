use alloc::boxed::Box;
use core::num::{NonZeroU32, NonZeroU64};

use capability::WaitToken;

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

impl From<ThreadId> for WaitToken {
    fn from(value: ThreadId) -> Self {
        WaitToken::new(NonZeroU64::new(u64::from(value.raw().get())).expect("ThreadId is nonzero"))
    }
}

impl TryFrom<WaitToken> for ThreadId {
    type Error = ();

    fn try_from(value: WaitToken) -> Result<Self, Self::Error> {
        let raw = u32::try_from(value.raw().get()).map_err(|_| ())?;
        let raw = NonZeroU32::new(raw).ok_or(())?;
        Ok(Self::new(raw))
    }
}

/// Идентификатор процесса.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct ProcessId(NonZeroU32);

impl ProcessId {
    pub const fn new(raw: NonZeroU32) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> NonZeroU32 {
        self.0
    }
}

/// Приоритет потока: 0 = наивысший, больше = ниже.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Priority(u8);

impl Priority {
    pub const HIGHEST: Self = Self(0);
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

/// Выбор адресного пространства для нового kernel-thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnAddressSpace {
    Inherit,
    Kernel,
    User,
}

/// Конфигурация нового kernel-thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnConfig {
    pub name: &'static str,
    pub priority: Priority,
    pub stack_pages: usize,
    pub address_space: SpawnAddressSpace,
}

impl SpawnConfig {
    pub const DEFAULT_STACK_PAGES: usize = 4;

    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            priority: Priority::NORMAL,
            stack_pages: Self::DEFAULT_STACK_PAGES,
            address_space: SpawnAddressSpace::Kernel,
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

    pub const fn address_space(mut self, address_space: SpawnAddressSpace) -> Self {
        self.address_space = address_space;
        self
    }
}

/// Ошибки запуска и управления потоком.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnError {
    NoFreeThreadSlots,
    InvalidName,
    InvalidPriority,
    InvalidStackPages,
    StackAllocationFailed,
    AddressSpaceCreationFailed,
    ImageNotLoaded,
}

impl From<SpawnError> for capability::SpawnError {
    fn from(value: SpawnError) -> Self {
        match value {
            SpawnError::NoFreeThreadSlots => Self::NoFreeThreadSlots,
            SpawnError::InvalidName => Self::InvalidName,
            SpawnError::InvalidPriority => Self::InvalidPriority,
            SpawnError::InvalidStackPages => Self::InvalidStackPages,
            SpawnError::StackAllocationFailed => Self::StackAllocationFailed,
            SpawnError::AddressSpaceCreationFailed => Self::AddressSpaceCreationFailed,
            SpawnError::ImageNotLoaded => Self::ImageNotLoaded,
        }
    }
}

/// Контракт сервиса планировщика.
pub trait SchedulerService: Send + Sync {
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

/// Удобные generic-обёртки над boxed [`SchedulerService`].
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
