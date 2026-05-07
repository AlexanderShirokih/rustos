#![allow(unsafe_code)]

use alloc::boxed::Box;
use core::{marker::PhantomData, mem::size_of, ptr::NonNull};

use memory::memory_mapper::AddressSpaceHandle;

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

/// Магическое значение, размещаемое в первых 16 байтах стека (canary).
///
/// Проверяется планировщиком при каждом переключении контекста: повреждение
/// сигнализирует о переполнении стека вниз.
///
/// Хранится как `[u8; 16]` (а не `[u64; 2]`), чтобы избежать требований к
/// выравниванию `Box<[u8]>` (align=1).
const STACK_CANARY_VALUE: u128 = 0xDEAD_C0DE_FACE_FEED_BEEF_BAD0_DEAD_F00Du128;

/// Размер canary в байтах.
pub const STACK_CANARY_SIZE: usize = size_of::<u128>();

/// Байтовое представление canary в little-endian.
pub const STACK_CANARY: [u8; STACK_CANARY_SIZE] = STACK_CANARY_VALUE.to_le_bytes();

struct PreemptionGuard<C: ArchCpu>(PhantomData<C>);

impl<C: ArchCpu> Drop for PreemptionGuard<C> {
    fn drop(&mut self) {
        C::enable_preemption();
    }
}

/// Выполняет `f` при замаскированном preemption/IRQ и гарантированно
/// восстанавливает предыдущее состояние при выходе из scope.
pub fn with_preemption_disabled<C: ArchCpu, R>(f: impl FnOnce() -> R) -> R {
    C::disable_preemption();
    let _guard = PreemptionGuard::<C>(PhantomData);
    f()
}

/// Владеющий стек потока с canary в начале (low addresses) и опциональным
/// cleanup hook.
pub struct ThreadStack {
    bytes: Box<[u8]>,
    top: NonNull<u8>,
    canary_addr: NonNull<u8>,
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
        mut bytes: Box<[u8]>,
        cleanup: Option<Box<dyn FnOnce() + Send + 'static>>,
    ) -> Result<Self, StackError> {
        if bytes.len() < STACK_CANARY_SIZE {
            return Err(StackError::InvalidSize);
        }

        let base = bytes.as_mut_ptr();
        // SAFETY: запись STACK_CANARY_SIZE байт в начало живого Box<[u8]>; границы
        // проверены выше через bytes.len() >= STACK_CANARY_SIZE.
        unsafe {
            core::ptr::copy_nonoverlapping(STACK_CANARY.as_ptr(), base, STACK_CANARY_SIZE);
        }
        // SAFETY: base != null, так как Box<[u8]> жив и не пуст.
        let canary_addr = unsafe { NonNull::new_unchecked(base) };
        let top = NonNull::new(base.wrapping_add(bytes.len())).ok_or(StackError::InvalidSize)?;

        Ok(Self {
            bytes,
            top,
            canary_addr,
            cleanup,
        })
    }

    pub fn top(&self) -> NonNull<u8> {
        self.top
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Возвращает `true`, если стек пуст. Введено для согласованности с
    /// `len()` и удовлетворения линтера clippy::len_without_is_empty.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Возвращает `true`, если canary в основании стека не повреждён.
    pub fn check_canary(&self) -> bool {
        let mut actual = [0u8; STACK_CANARY_SIZE];
        // SAFETY: canary_addr указывает в начало живого Box<[u8]> длиной >= STACK_CANARY_SIZE.
        unsafe {
            core::ptr::copy_nonoverlapping(
                self.canary_addr.as_ptr(),
                actual.as_mut_ptr(),
                STACK_CANARY_SIZE,
            );
        }
        actual == STACK_CANARY
    }

    /// Адрес начала стека (low addresses) для диагностических сообщений.
    pub fn base_addr(&self) -> usize {
        self.canary_addr.as_ptr() as usize
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
    type Stack: ThreadStackAllocator;

    fn init(stack_top: NonNull<u8>, entry: TrampolineFn, arg: *mut ()) -> Self;

    /// Не возвращается: первый прыжок в стек выбранного потока.
    ///
    /// # Safety
    /// Должен вызываться ровно один раз на CPU при замаскированных IRQ. `next`
    /// должен указывать на инициализированный контекст, валидный пока поток жив.
    unsafe fn start(next: &Self) -> !;

    /// Сохраняет регистры `prev` и восстанавливает регистры `next`.
    ///
    /// # Safety
    /// Вызывается только scheduler-ом под scheduler-lock-ом при эксклюзивном
    /// владении обоими контекстами. Оба указателя должны указывать на живые
    /// `ThreadStack`-владеемые контексты.
    unsafe fn switch(prev: &mut Self, next: &Self);

    /// Активирует user-AS, описанный `next` для текущего CPU; `None` -
    /// kernel-only режим (user-mapping недоступен). Реализация обязана
    /// гарантировать, что после возврата трансляции прошлого AS не будут
    /// видимы для текущего CPU.
    ///
    /// Дефолтная реализация пуста: host-тесты с mock-арх не затрагивают MMU.
    fn switch_address_space(_next: Option<AddressSpaceHandle>) {}
}

/// Операции CPU, требуемые scheduler.
pub trait ArchCpu: Send + Sync + 'static {
    fn current_id() -> CpuId;

    /// Сохраняет указатель на per-CPU состояние (`Cpu`) в архитектурно-определённом
    /// CPU-local регистре.
    ///
    /// # Safety
    /// Указатель должен жить всё время жизни CPU. После вызова scheduler начинает
    /// читать его через [`ArchCpu::cpu_local_ptr`].
    unsafe fn install_cpu_local(_cpu: *mut ()) {}

    /// Возвращает ранее установленный через [`ArchCpu::install_cpu_local`] указатель,
    /// либо `core::ptr::null_mut()`, если установка ещё не была выполнена.
    fn cpu_local_ptr() -> *mut () {
        core::ptr::null_mut()
    }

    fn idle() -> !;

    fn enable_preemption() {}

    fn disable_preemption() {}
}

/// Выделение стека потока.
pub trait ThreadStackAllocator: Send + Sync + 'static {
    fn allocate(pages: usize) -> Result<ThreadStack, StackError>;
}

/// Источник монотонного времени.
pub trait TimerSource: Send + Sync + 'static {
    fn now_ns(&self) -> u64;

    fn schedule_next(&self, deadline_ns: u64);
}
