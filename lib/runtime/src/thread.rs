//! Запуск потока в текущем процессе: стек из кучи процесса и вход по трамплину вызывающего.
//!
//! Механизм - выдача стека и `ThreadCreate`, а что делает поток на выходе, решает трамплин
//! вызывающего. Поверх механизма лежит `spawn`. Он берёт тело потока значением и снимает с
//! вызывающего передачу состояния через сырой указатель.

use alloc::boxed::Box;
use core::ptr::NonNull;

use syscall::{Handle, Priority, SIGNALED, SyscallError, Timeout, kind};

use crate::{
    heap::{alloc, dealloc},
    svc::{
        handle_close, process_self, signal_wait_one, thread_create, thread_exit, thread_exit_code,
    },
    tls::run_destructors,
};

const STACK_ALIGN: usize = 16;

/// Поднятый поток: его handle и стек, выданный кучей процесса.
pub struct Spawned {
    thread: Handle<kind::Thread>,
    stack: NonNull<u8>,
    stack_bytes: usize,
}

impl Spawned {
    /// Handle поднятого потока.
    #[must_use]
    pub const fn thread(&self) -> Handle<kind::Thread> {
        self.thread
    }

    /// База стека потока.
    #[must_use]
    pub const fn stack(&self) -> NonNull<u8> {
        self.stack
    }

    /// Размер стека потока в байтах.
    #[must_use]
    pub const fn stack_bytes(&self) -> usize {
        self.stack_bytes
    }
}

/// Поднимает поток в текущем процессе. Выделяет стек `stack_bytes` и входит в `entry(arg)` с
/// приоритетом `priority`.
///
/// На отказе выделенный стек возвращается куче.
pub fn create(
    entry: extern "C" fn(usize) -> !,
    arg: usize,
    stack_bytes: usize,
    priority: Priority,
) -> Result<Spawned, SyscallError> {
    let bytes = stack_bytes
        .max(STACK_ALIGN)
        .checked_next_multiple_of(STACK_ALIGN)
        .ok_or(SyscallError::InvalidArgument)?;
    // SAFETY: размер ненулевой по границе снизу, выравнивание - степень двойки.
    let stack =
        NonNull::new(unsafe { alloc(bytes, STACK_ALIGN) }).ok_or(SyscallError::OutOfMemory)?;

    let process = match process_self() {
        Ok(process) => process,
        Err(error) => {
            // SAFETY: стек только что выдан этой кучей и никем не адресуется.
            unsafe { release_stack(stack, bytes) };
            return Err(error);
        }
    };
    let created = thread_create(
        process,
        entry as *const () as u64,
        stack.addr().get() as u64 + bytes as u64,
        arg as u64,
        u64::from(priority),
    );
    // `ProcessSelf` ставит свежую запись таблицы handle на каждый вызов.
    handle_close(process);

    match created {
        Ok(thread) => Ok(Spawned {
            thread,
            stack,
            stack_bytes: bytes,
        }),
        Err(error) => {
            // SAFETY: поток не стартовал, стек остался только у вызывающего.
            unsafe { release_stack(stack, bytes) };
            Err(error)
        }
    }
}

/// Возвращает стек потока куче процесса.
///
/// # Safety
///
/// Поток терминирован, его стек больше никем не адресуется, а `stack` и `stack_bytes` взяты
/// у одного [`Spawned`].
pub unsafe fn release_stack(stack: NonNull<u8>, stack_bytes: usize) {
    // SAFETY: блок выдан кучей процесса с этим размером и выравниванием.
    unsafe { dealloc(stack.as_ptr(), stack_bytes, STACK_ALIGN) };
}

struct SpawnPayload {
    entry: extern "C" fn(usize) -> u32,
    arg: usize,
}

extern "C" fn trampoline(payload: usize) -> ! {
    // SAFETY: payload получен из Box::into_raw в spawn и реконструируется ровно
    // один раз; разыменование освобождает аллокацию до расходящегося thread_exit.
    let SpawnPayload { entry, arg } = *unsafe { Box::from_raw(payload as *mut SpawnPayload) };
    let code = entry(arg);
    // Уход потока - последний момент, когда его локальные данные ещё адресуемы.
    run_destructors();
    thread_exit(u64::from(code))
}

/// Запускает поток в текущем процессе с телом `body`. Владение перенесённым в
/// него состоянием берёт на себя поток: `spawn_owned` снимает с вызывающего
/// передачу состояния через сырой указатель.
pub fn spawn_owned<F>(
    body: F,
    stack_bytes: u64,
    priority: Priority,
) -> Result<JoinHandle, SyscallError>
where
    F: FnOnce() -> u32 + Send + 'static,
{
    let body: Box<dyn FnOnce() -> u32> = Box::new(body);
    let payload = Box::into_raw(Box::new(body)) as usize;
    match spawn(run_owned, payload, stack_bytes, priority) {
        Ok(thread) => Ok(thread),
        Err(error) => {
            // Поток не стартовал - тело его не прочитает.
            // SAFETY: указатель только что из Box::into_raw, нет других владельцев.
            drop(unsafe { Box::from_raw(payload as *mut Box<dyn FnOnce() -> u32>) });
            Err(error)
        }
    }
}

extern "C" fn run_owned(payload: usize) -> u32 {
    // SAFETY: payload получен из Box::into_raw в spawn_owned и реконструируется
    // ровно один раз: этот поток - единственный его владелец.
    let body = *unsafe { Box::from_raw(payload as *mut Box<dyn FnOnce() -> u32>) };
    body()
}

/// Запускает поток в текущем процессе. Выделяет стек `stack_bytes` и входит в
/// `entry(arg)` с приоритетом `priority`. Поток завершается кодом возврата `entry`.
pub fn spawn(
    entry: extern "C" fn(usize) -> u32,
    arg: usize,
    stack_bytes: u64,
    priority: Priority,
) -> Result<JoinHandle, SyscallError> {
    let payload = Box::into_raw(Box::new(SpawnPayload { entry, arg })) as usize;
    let bytes = usize::try_from(stack_bytes).unwrap_or(usize::MAX);
    match create(trampoline, payload, bytes, priority) {
        Ok(spawned) => Ok(JoinHandle {
            thread: spawned.thread(),
            stack: spawned.stack(),
            stack_bytes: spawned.stack_bytes(),
        }),
        Err(error) => {
            // Поток не стартовал - трамплин payload не прочитает, освобождаем его;
            // стек вернул куче сам слой.
            // SAFETY: payload только что из Box::into_raw, нет других владельцев.
            drop(unsafe { Box::from_raw(payload as *mut SpawnPayload) });
            Err(error)
        }
    }
}

/// Дескриптор запущенного потока владеет его хэндлом и стеком.
///
/// `join` ждёт терминации и возвращает стек куче. `Drop` закрывает хэндл, а уход без `join`
/// оставляет стек выданным: поток мог ещё выполняться на нём.
pub struct JoinHandle {
    thread: Handle<kind::Thread>,
    stack: NonNull<u8>,
    stack_bytes: usize,
}

// SAFETY: поля описывают чужой поток и его стек, и владение ими переносится целиком; стек
// адресуется только при освобождении, когда поток уже терминирован.
unsafe impl Send for JoinHandle {}
// SAFETY: см. impl Send - по общей ссылке стек недоступен, а операции над потоком идут через
// его собственный хэндл.
unsafe impl Sync for JoinHandle {}

impl JoinHandle {
    pub const fn handle(&self) -> Handle<kind::Thread> {
        self.thread
    }

    /// Ждёт терминации потока до `timeout`.
    pub fn wait(&self, timeout: Timeout) -> Result<(), SyscallError> {
        let observed = signal_wait_one(self.thread, SIGNALED, timeout);
        if observed < 0 {
            return Err(SyscallError::from_return(observed).unwrap_or(SyscallError::Canceled));
        }
        Ok(())
    }

    /// Ждёт терминации потока до `timeout` и возвращает его стек куче.
    ///
    /// Отказ ожидания потребляет `self`: `Drop` закрывает хэндл, а стек куче не
    /// возвращается.
    pub fn join(self, timeout: Timeout) -> Result<(), SyscallError> {
        self.wait(timeout)?;
        // SAFETY: поток терминирован, и стек больше никем не адресуется; указатель и размер
        // взяты у одного запуска.
        unsafe { release_stack(self.stack, self.stack_bytes) };
        Ok(())
    }

    /// Финальный exit-код завершённого потока.
    pub fn exit_code(&self) -> Result<u32, SyscallError> {
        let code = thread_exit_code(self.thread);
        if code < 0 {
            return Err(SyscallError::from_return(code).unwrap_or(SyscallError::BadHandle));
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Ok((code as u64 & 0xFFFF_FFFF) as u32)
    }
}

impl Drop for JoinHandle {
    fn drop(&mut self) {
        handle_close(self.thread);
    }
}
