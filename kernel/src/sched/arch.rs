use alloc::boxed::Box;
use core::ptr::NonNull;

/// Идентификатор CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct CpuId(u16);

impl CpuId {
    pub const fn new(raw: u16) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u16 {
        self.0
    }

    pub const fn as_index(self) -> usize {
        self.0 as usize
    }
}

/// Ошибки выделения стека.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackError {
    InvalidSize,
    OutOfMemory,
    Unsupported,
}

/// Функция trampoline для первого запуска потока.
pub type TrampolineFn = unsafe extern "C" fn(arg: *mut ()) -> !;

/// Владеющий стек потока с опциональным cleanup hook.
pub struct ThreadStack {
    bytes: Box<[u8]>,
    top: NonNull<u8>,
    cleanup: Option<Box<dyn FnOnce() + Send + 'static>>,
}

// SAFETY: ThreadStack владеет памятью стека и не предоставляет aliasing access к её содержимому.
unsafe impl Send for ThreadStack {}
// SAFETY: доступ к стеку синхронизируется владением Thread в scheduler, cached pointer read-only.
unsafe impl Sync for ThreadStack {}

impl ThreadStack {
    pub fn from_boxed_bytes(bytes: Box<[u8]>) -> Result<Self, StackError> {
        Self::with_cleanup(bytes, None)
    }

    pub fn with_cleanup(
        bytes: Box<[u8]>,
        cleanup: Option<Box<dyn FnOnce() + Send + 'static>>,
    ) -> Result<Self, StackError> {
        if bytes.is_empty() {
            return Err(StackError::InvalidSize);
        }

        let base = bytes.as_ptr() as *mut u8;
        let top = NonNull::new(base.wrapping_add(bytes.len())).ok_or(StackError::InvalidSize)?;

        Ok(Self {
            bytes,
            top,
            cleanup,
        })
    }

    pub fn top(&self) -> NonNull<u8> {
        self.top
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }
}

impl Drop for ThreadStack {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            cleanup();
        }
    }
}

/// Архитектурно-зависимый контекст потока.
pub trait ArchContext: Sized + Send + 'static {
    type Cpu: ArchCpu;
    type Stack: ArchStack;

    fn init(stack_top: NonNull<u8>, entry: TrampolineFn, arg: *mut ()) -> Self;

    /// SAFETY: используется только для первого входа в первый поток и не возвращается.
    unsafe fn start(next: &Self) -> !;

    /// SAFETY: вызывается только scheduler при эксклюзивном владении обоими контекстами.
    unsafe fn switch(prev: &mut Self, next: &Self);
}

/// Операции CPU, требуемые scheduler.
pub trait ArchCpu: Send + Sync + 'static {
    fn current_id() -> CpuId;

    /// SAFETY: указатель должен жить всё время жизни CPU-local state.
    unsafe fn install_cpu_local(_cpu: *mut ()) {}

    fn idle() -> !;

    fn enable_preemption() {}

    fn disable_preemption() {}
}

/// Выделение архитектурного стека потока.
pub trait ArchStack: Send + Sync + 'static {
    fn allocate(pages: usize) -> Result<ThreadStack, StackError>;
}

/// Источник монотонного времени и программирование следующего тика.
pub trait TimerSource: Send + Sync + 'static {
    fn now_ns(&self) -> u64;

    fn schedule_next(&self, deadline_ns: u64);
}
