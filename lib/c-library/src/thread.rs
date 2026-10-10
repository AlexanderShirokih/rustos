//! Потоки POSIX библиотеки C: запись потока, его жизненный цикл и атрибуты.
//!
//! `pthread_t` - адрес записи потока. Запись хранит `errno` потока, значение выхода, состояние
//! отсоединения, `JoinHandle` потока, массив его экземпляров thread-local переменных emutls и
//! список деструкторов `thread_local`.
//! Поток находит свою запись по ключу идентичности `runtime::tls`, пустой слот означает главный
//! поток со статической записью.
//!
//! Деструкторы `thread_local` выполняются при выходе потока до деструкторов его ключей, а при
//! `exit` - для вызвавшего потока.
//!
//! Запись существует до терминации потока, в том числе во время деструкторов его ключей, и
//! освобождается вместе с массивом экземпляров.
//! Запись отсоединённого потока при выходе встаёт в список завершившихся. Список проходят
//! `pthread_create`, `pthread_join` и `pthread_detach` и освобождают записи терминированных
//! потоков.

#![allow(unsafe_code)]

use alloc::{boxed::Box, vec::Vec};
use core::{
    cell::UnsafeCell,
    ffi::{c_int, c_void},
    mem::MaybeUninit,
    ptr,
    sync::atomic::{
        AtomicI32, AtomicPtr, AtomicU32, AtomicUsize,
        Ordering::{AcqRel, Acquire, Relaxed, Release},
    },
};

use runtime::{JoinHandle, Mutex, Timeout, thread::Builder, tls};
use syscall::PAGE_SIZE;

use crate::{emutls::Instances, errno::Errno, process};

type Start = unsafe extern "C" fn(*mut c_void) -> *mut c_void;

type Destructor = unsafe extern "C" fn(*mut c_void);

const JOINABLE: u32 = 0;
const DETACHED: u32 = 1;
const EXITING: u32 = 2;

const DEFAULT_STACK_BYTES: usize = 256 * 1024;

const _: () = assert!(size_of::<Attributes>() == 16 && align_of::<Attributes>() == 8);

// Значения определены в `pthread.c` константами и не меняются.
unsafe extern "C" {
    safe static c_library_pthread_create_joinable: c_int;
    safe static c_library_pthread_create_detached: c_int;
    safe static c_library_pthread_stack_min: usize;
}

static MAIN: Record = Record::new(JOINABLE, None, ptr::null_mut());

static IDENTITY: AtomicUsize = AtomicUsize::new(0);

static EXITED: Mutex<Exited> = Mutex::new(Exited { head: ptr::null() });

#[used]
#[unsafe(link_section = ".fini_thread_local")]
static FINI_THREAD_LOCAL: extern "C" fn() = run_current_thread_locals;

pub(crate) struct Record {
    errno: AtomicI32,
    value: AtomicPtr<c_void>,
    state: AtomicU32,
    handle: Mutex<Option<JoinHandle>>,
    next: AtomicPtr<Record>,
    start: Option<Start>,
    arg: *mut c_void,
    instances: Instances,
    destructors: UnsafeCell<Vec<(Destructor, *mut c_void)>>,
}

#[repr(C)]
struct Attributes {
    stack_bytes: usize,
    detach_state: c_int,
}

struct Exited {
    head: *const Record,
}

// SAFETY: `start` и `arg` пишет создатель до запуска потока и затем только читает сам поток;
// массив экземпляров и список деструкторов меняет только сам поток; остальные поля атомарны
// или под мьютексом.
unsafe impl Sync for Record {}

// SAFETY: список хранит адреса записей из кучи процесса, доступные любому потоку; звенья
// меняются только под мьютексом списка.
unsafe impl Send for Exited {}

impl Record {
    const fn new(state: u32, start: Option<Start>, arg: *mut c_void) -> Self {
        Self {
            errno: AtomicI32::new(0),
            value: AtomicPtr::new(ptr::null_mut()),
            state: AtomicU32::new(state),
            handle: Mutex::new(None),
            next: AtomicPtr::new(ptr::null_mut()),
            start,
            arg,
            instances: Instances::new(),
            destructors: UnsafeCell::new(Vec::new()),
        }
    }

    pub(crate) const fn errno(&self) -> *mut c_int {
        self.errno.as_ptr()
    }

    pub(crate) const fn instances(&self) -> &Instances {
        &self.instances
    }

    unsafe fn push_destructor(&self, destructor: Destructor, object: *mut c_void) -> bool {
        // SAFETY: вызывающий - поток записи; список меняет только он, и ссылка не покидает
        // вызов.
        let destructors = unsafe { &mut *self.destructors.get() };
        if destructors.try_reserve(1).is_err() {
            return false;
        }
        destructors.push((destructor, object));
        true
    }

    unsafe fn run_destructors(&self) {
        loop {
            // SAFETY: вызывающий - поток записи; ссылка на список не покидает выражение и не
            // держится во время деструктора, который может пополнить список.
            let Some((destructor, object)) = (unsafe { &mut *self.destructors.get() }).pop() else {
                return;
            };
            // SAFETY: пару передал `__cxa_thread_atexit_impl` по контракту C++ ABI.
            unsafe { destructor(object) };
        }
    }

    fn take_terminated(&self) -> Option<JoinHandle> {
        let mut handle = self.handle.lock();
        if handle
            .as_ref()
            .is_some_and(|handle| handle.wait(Timeout::POLL).is_ok())
        {
            return handle.take();
        }
        None
    }
}

impl Attributes {
    fn new() -> Self {
        Self {
            stack_bytes: DEFAULT_STACK_BYTES,
            detach_state: c_library_pthread_create_joinable,
        }
    }
}

impl Exited {
    fn push(&mut self, record: &Record) {
        record.next.store(self.head.cast_mut(), Relaxed);
        self.head = record;
    }

    fn reap(&mut self) {
        let mut previous: *const Record = ptr::null();
        let mut cursor = self.head;
        // SAFETY: звенья списка - записи из `pthread_create`, их освобождает только этот обход
        // под мьютексом списка.
        while let Some(record) = unsafe { cursor.as_ref() } {
            let next = record.next.load(Relaxed);
            let Some(handle) = record.take_terminated() else {
                previous = cursor;
                cursor = next;
                continue;
            };

            let _ = handle.join(Timeout::POLL);
            // SAFETY: `previous` - звено того же списка, освобождаемое не раньше `cursor`.
            match unsafe { previous.as_ref() } {
                Some(previous) => previous.next.store(next, Relaxed),
                None => self.head = next,
            }
            // SAFETY: поток записи терминирован, запись выдана `Box` в `pthread_create` и
            // вынута из списка - на неё нет других ссылок.
            drop(unsafe { Box::from_raw(cursor.cast_mut()) });
            cursor = next;
        }
    }
}

pub(crate) fn install_identity() {
    let key = tls::create(None);
    if key == 0 {
        process::abort_with("thread identity key is unavailable");
    }
    IDENTITY.store(key, Relaxed);
}

#[allow(clippy::cast_ptr_alignment)]
pub(crate) fn current() -> &'static Record {
    let key = IDENTITY.load(Relaxed);
    if key == 0 {
        return &MAIN;
    }

    // SAFETY: ключ выдан `tls::create` в `install_identity`.
    let record = unsafe { tls::get(key) }.cast::<Record>();
    // SAFETY: непустой слот хранит адрес записи этого потока из `Box`, выровненный под
    // `Record`, и запись существует до терминации потока.
    unsafe { record.as_ref() }.unwrap_or(&MAIN)
}

extern "C" fn entry(record: usize) -> u32 {
    let record = record as *mut Record;
    let key = IDENTITY.load(Relaxed);
    // SAFETY: ключ выдан `tls::create` в `install_identity`.
    unsafe { tls::set(key, record.cast()) };
    // SAFETY: см. выше.
    if unsafe { tls::get(key) } != record.cast() {
        process::abort_with("thread record is not installed");
    }

    // SAFETY: запись выдана `pthread_create` и существует до терминации этого потока.
    let record = unsafe { &*record };
    let value = match record.start {
        // SAFETY: `start` и `arg` переданы `pthread_create` по контракту C.
        Some(start) => unsafe { start(record.arg) },
        None => ptr::null_mut(),
    };
    finish(record, value)
}

fn finish(record: &Record, value: *mut c_void) -> ! {
    if ptr::eq(record, &raw const MAIN) {
        process::abort_with("pthread_exit called from the main thread");
    }

    // SAFETY: `finish` исполняет поток записи.
    unsafe { record.run_destructors() };
    record.value.store(value, Release);
    if record
        .state
        .compare_exchange(JOINABLE, EXITING, AcqRel, Acquire)
        .is_err()
    {
        EXITED.lock().push(record);
    }
    runtime::thread::exit(0)
}

fn reap() {
    EXITED.lock().reap();
}

extern "C" fn run_current_thread_locals() {
    // SAFETY: запись `current` принадлежит вызывающему потоку.
    unsafe { current().run_destructors() };
}

#[unsafe(no_mangle)]
unsafe extern "C" fn __cxa_thread_atexit_impl(
    destructor: Destructor,
    object: *mut c_void,
    _dso_symbol: *mut c_void,
) -> c_int {
    // SAFETY: запись `current` принадлежит вызывающему потоку.
    if unsafe { current().push_destructor(destructor, object) } {
        0
    } else {
        -1
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pthread_create(
    thread: *mut *const Record,
    attr: Option<&Attributes>,
    start: Option<Start>,
    arg: *mut c_void,
) -> c_int {
    reap();

    let attributes = attr.map_or_else(Attributes::new, |attr| Attributes {
        stack_bytes: attr.stack_bytes,
        detach_state: attr.detach_state,
    });
    if start.is_none() {
        return Errno::InvalidArgument.code();
    }

    let state = if attributes.detach_state == c_library_pthread_create_detached {
        DETACHED
    } else {
        JOINABLE
    };
    let Ok(record) = Box::try_new(Record::new(state, start, arg)) else {
        return Errno::Again.code();
    };
    let record = Box::into_raw(record);

    let Ok(handle) = Builder::new()
        .stack_bytes(attributes.stack_bytes)
        .spawn(entry, record.addr())
    else {
        // SAFETY: поток не стартовал, запись есть только у создателя.
        drop(unsafe { Box::from_raw(record) });
        return Errno::Again.code();
    };
    // SAFETY: запись освобождается только после того, как в неё поставлен `JoinHandle`.
    *unsafe { &*record }.handle.lock() = Some(handle);

    // SAFETY: `thread` указывает на `pthread_t` по контракту C.
    unsafe { thread.write(record) };
    0
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pthread_join(thread: *const Record, value: *mut *mut c_void) -> c_int {
    if ptr::eq(thread, current()) {
        return Errno::Deadlock.code();
    }

    // SAFETY: `thread` выдан `pthread_create` и не присоединён, по контракту C.
    let Some(record) = (unsafe { thread.as_ref() }) else {
        return Errno::InvalidArgument.code();
    };
    if record.state.load(Acquire) == DETACHED {
        return Errno::InvalidArgument.code();
    }
    let Some(handle) = record.handle.lock().take() else {
        return Errno::InvalidArgument.code();
    };
    if handle.join(Timeout::INFINITE).is_err() {
        return Errno::InvalidArgument.code();
    }

    let result = record.value.load(Acquire);
    // SAFETY: поток терминирован, запись выдана `Box` в `pthread_create`, и присоединяет её
    // один вызывающий.
    drop(unsafe { Box::from_raw(thread.cast_mut()) });
    if !value.is_null() {
        // SAFETY: непустой `value` указывает на `void *` по контракту C.
        unsafe { value.write(result) };
    }
    reap();
    0
}

#[unsafe(no_mangle)]
unsafe extern "C" fn pthread_detach(thread: *const Record) -> c_int {
    // SAFETY: `thread` выдан `pthread_create` и не присоединён, по контракту C.
    let Some(record) = (unsafe { thread.as_ref() }) else {
        return Errno::InvalidArgument.code();
    };

    if record
        .state
        .compare_exchange(JOINABLE, DETACHED, AcqRel, Acquire)
        .is_ok()
    {
        return 0;
    }

    if record
        .state
        .compare_exchange(EXITING, DETACHED, AcqRel, Acquire)
        .is_err()
    {
        return Errno::InvalidArgument.code();
    }
    let mut exited = EXITED.lock();
    exited.push(record);
    exited.reap();
    0
}

#[unsafe(no_mangle)]
extern "C" fn pthread_exit(value: *mut c_void) -> ! {
    finish(current(), value)
}

#[unsafe(no_mangle)]
extern "C" fn sched_yield() -> c_int {
    runtime::thread_yield();
    0
}

#[unsafe(no_mangle)]
extern "C" fn pthread_self() -> *const Record {
    current()
}

#[unsafe(no_mangle)]
extern "C" fn pthread_equal(first: *const Record, second: *const Record) -> c_int {
    c_int::from(ptr::eq(first, second))
}

#[unsafe(no_mangle)]
extern "C" fn pthread_attr_init(attr: Option<&mut MaybeUninit<Attributes>>) -> c_int {
    let Some(attr) = attr else {
        return Errno::InvalidArgument.code();
    };

    attr.write(Attributes::new());
    0
}

#[unsafe(no_mangle)]
extern "C" fn pthread_attr_destroy(_attr: *mut Attributes) -> c_int {
    0
}

#[unsafe(no_mangle)]
extern "C" fn pthread_attr_setstacksize(
    attr: Option<&mut Attributes>,
    stack_bytes: usize,
) -> c_int {
    let Some(attr) = attr else {
        return Errno::InvalidArgument.code();
    };

    if stack_bytes < c_library_pthread_stack_min {
        return Errno::InvalidArgument.code();
    }
    let Some(rounded) = stack_bytes.checked_next_multiple_of(PAGE_SIZE) else {
        return Errno::InvalidArgument.code();
    };

    attr.stack_bytes = rounded;
    0
}

#[unsafe(no_mangle)]
extern "C" fn pthread_attr_getstacksize(
    attr: Option<&Attributes>,
    stack_bytes: Option<&mut usize>,
) -> c_int {
    let (Some(attr), Some(stack_bytes)) = (attr, stack_bytes) else {
        return Errno::InvalidArgument.code();
    };

    *stack_bytes = attr.stack_bytes;
    0
}

#[unsafe(no_mangle)]
extern "C" fn pthread_attr_setdetachstate(
    attr: Option<&mut Attributes>,
    detach_state: c_int,
) -> c_int {
    let Some(attr) = attr else {
        return Errno::InvalidArgument.code();
    };

    if detach_state != c_library_pthread_create_joinable
        && detach_state != c_library_pthread_create_detached
    {
        return Errno::InvalidArgument.code();
    }

    attr.detach_state = detach_state;
    0
}

#[unsafe(no_mangle)]
extern "C" fn pthread_attr_getdetachstate(
    attr: Option<&Attributes>,
    detach_state: Option<&mut c_int>,
) -> c_int {
    let (Some(attr), Some(detach_state)) = (attr, detach_state) else {
        return Errno::InvalidArgument.code();
    };

    *detach_state = attr.detach_state;
    0
}
