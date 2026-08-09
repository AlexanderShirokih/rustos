//! Запуск потока в текущем процессе: выделение стека и вход через трамплин.

use alloc::boxed::Box;
use core::mem::ManuallyDrop;

use molecular_rt::tls::run_destructors;
use syscall::{Timeout, UserMemFlags};

use crate::{
    error::Result,
    handle::BorrowedHandle,
    object::{AnonymousMapping, Priority, Process, Resource, Thread, ThreadEntry},
    svc::thread_exit,
};

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
pub fn spawn_owned<F>(body: F, stack_bytes: u64, priority: Priority) -> Result<JoinHandle>
where
    F: FnOnce() -> u32 + Send + 'static,
{
    let body: Box<dyn FnOnce() -> u32> = Box::new(body);
    let payload = Box::into_raw(Box::new(body)) as usize;
    match spawn(run_owned, payload, stack_bytes, priority) {
        Ok(thread) => Ok(thread),
        Err(error) => {
            // Поток не стартовал - тело его не прочитает.
            // SAFETY: указатель только что из Box::into_raw, других владельцев нет.
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

/// Запускает поток в текущем процессе: выделяет стек `stack_bytes` и входит в
/// `entry(arg)` с приоритетом `priority`. Поток завершается кодом возврата `entry`.
pub fn spawn(
    entry: extern "C" fn(usize) -> u32,
    arg: usize,
    stack_bytes: u64,
    priority: Priority,
) -> Result<JoinHandle> {
    let stack = Resource::self_resource()?.allocate(stack_bytes, UserMemFlags::ReadWrite)?;
    let user_sp = stack.va() + stack_bytes;
    let process = Process::self_process()?;

    let payload = Box::into_raw(Box::new(SpawnPayload { entry, arg })) as usize;
    let trampoline_pc: extern "C" fn(usize) -> ! = trampoline;
    let thread = Thread::create(
        process.handle(),
        ThreadEntry {
            entry_pc: trampoline_pc as *const () as u64,
            user_sp,
            arg: payload as u64,
            priority,
        },
    );

    match thread {
        Ok(thread) => Ok(JoinHandle {
            thread,
            stack: ManuallyDrop::new(stack),
        }),
        Err(err) => {
            // Поток не стартовал - трамплин payload не прочитает, освобождаем его;
            // стек освободит свой Drop.
            // SAFETY: payload только что из Box::into_raw, других владельцев нет.
            drop(unsafe { Box::from_raw(payload as *mut SpawnPayload) });
            Err(err)
        }
    }
}

/// Дескриптор запущенного потока: владеет его хэндлом и стеком.
///
/// `join` ждёт терминации и освобождает стек. `Drop` без `join` отвязывает
/// поток: стек утекает: поток мог ещё выполняться на нём.
pub struct JoinHandle {
    thread: Thread,
    stack: ManuallyDrop<AnonymousMapping>,
}

impl JoinHandle {
    /// Заимствование хэндла потока на время одного вызова.
    pub fn handle(&self) -> BorrowedHandle<'_> {
        self.thread.handle()
    }

    /// Ждёт терминации потока до `timeout`, затем освобождает его стек.
    pub fn join(mut self, timeout: Timeout) -> Result<()> {
        self.thread.join(timeout)?;
        // SAFETY: join вернул Ok - поток терминирован, стек больше не нужен;
        // take освобождает стек ровно один раз, остаток ManuallyDrop не дропается.
        let stack = unsafe { ManuallyDrop::take(&mut self.stack) };
        stack.free()
    }

    /// Поток за хэндлом. Его принимает [`exit_with_sibling`](crate::exit_with_sibling).
    pub const fn thread(&self) -> &Thread {
        &self.thread
    }

    /// Финальный exit-код завершённого потока.
    pub fn exit_code(&self) -> Result<u32> {
        self.thread.exit_code()
    }
}
