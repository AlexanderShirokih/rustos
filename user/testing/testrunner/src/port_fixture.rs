#![no_std]
#![no_main]
#![feature(thread_local)]
#![allow(unsafe_code)]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::multiple_unsafe_ops_per_block
)]

use core::{
    cell::Cell,
    sync::atomic::{AtomicU64, Ordering},
};

use fixture_protocol::{
    OP_ALLOCATE_PAGES, OP_CALL_SIBLING, OP_CLOSE_CAP, OP_DUPLICATE_TRANSFER_CAP, OP_EXIT,
    OP_HOLD_CAP, OP_HOLD_SELF, OP_JOIN_THREAD, OP_MAP_CHECK, OP_PROCESS_EXIT, OP_READ_SLOT,
    OP_RECV_SIBLING, OP_SPIN_SIBLING, OP_WRITE_SLOT, READ_REPLY_LEN, REPLY_ACCESS_DENIED,
    REPLY_ALLOCATE_FAILED, REPLY_BUDGET_EXHAUSTED, REPLY_DUPLICATE_FAILED, REPLY_HOLD_FAILED,
    REPLY_MAP_FAILED, REPLY_OK, REPLY_PATTERN_MISMATCH, REPLY_SPAWN_FAILED, REQUEST_LEN,
    decode_request,
};
use kernel_objects::{Error, Mapping, MemoryRegion, Port, Process, Reply, Resource, Signal, kind};
use runtime::{
    ExitCode, IssuedHandle, MemoryAccess, Timeout, UserMemFlags, process_exit, signal_set,
    spawn_owned, thread_exit,
};
use syscall::{Handle, Rights, SIGNALED, SyscallError, WakeCount};
use userland::USERLAND_PAGE_SIZE;

const PARK_TIMEOUT: Timeout = Timeout::from_secs(5);

mod fixture_protocol;
mod ipc_raw;

static SLOT: AtomicU64 = AtomicU64::new(0);

#[thread_local]
static TOUCHED: Cell<u64> = Cell::new(0);

const FAILED_EXIT_CODE: u64 = 101;

#[program_entry::process]
fn run(port: &Port) -> ExitCode {
    loop {
        let reply = match port.recv(Timeout::INFINITE) {
            Ok(Some(reply)) => reply,
            Err(Error::Syscall(SyscallError::SenderGone)) => thread_exit(0),
            _ => thread_exit(FAILED_EXIT_CODE),
        };

        let mut body = [0u8; REQUEST_LEN];
        let (len, ncaps) = ipc_raw::read_frame(&mut body);
        if len < REQUEST_LEN {
            thread_exit(FAILED_EXIT_CODE)
        }
        let Some((op, arg)) = decode_request(&body) else {
            thread_exit(FAILED_EXIT_CODE)
        };

        match op {
            OP_WRITE_SLOT => {
                SLOT.store(arg, Ordering::Release);
                answer(reply, &[REPLY_OK]);
            }
            OP_READ_SLOT => {
                let mut out = [0u8; READ_REPLY_LEN];
                out[..8].copy_from_slice(&SLOT.load(Ordering::Acquire).to_le_bytes());
                out[8..].copy_from_slice(&(&raw const SLOT as u64).to_le_bytes());
                answer(reply, &out);
            }
            OP_MAP_CHECK => {
                let cap = if ncaps == 1 {
                    ipc_raw::take_cap(0)
                } else {
                    None
                };
                answer(reply, &[map_check(cap, arg)]);
            }
            OP_HOLD_CAP => {
                let cap = if ncaps == 1 {
                    ipc_raw::take_cap(0)
                } else {
                    None
                };
                answer(reply, &[hold_cap(cap)]);
            }
            OP_HOLD_SELF => answer(reply, &[hold_self()]),
            OP_RECV_SIBLING => answer(reply, &[start_sibling(Sibling::Recv)]),
            OP_CALL_SIBLING => {
                let cap = if ncaps == 1 {
                    ipc_raw::take_cap(0)
                } else {
                    None
                };
                let status = cap.map_or(REPLY_SPAWN_FAILED, |port| {
                    start_sibling(Sibling::Call(Port::from(port.assume_kind())))
                });
                answer(reply, &[status]);
            }
            OP_SPIN_SIBLING => answer(reply, &[start_sibling(Sibling::Spin)]),
            OP_JOIN_THREAD => answer(reply, &[join_thread()]),
            OP_CLOSE_CAP => {
                let Some(handle) = (if ncaps == 1 {
                    ipc_raw::take_cap(0)
                } else {
                    None
                }) else {
                    thread_exit(FAILED_EXIT_CODE)
                };
                drop(handle);
                answer(reply, &[REPLY_OK]);
            }
            OP_ALLOCATE_PAGES => answer(reply, &[allocate_pages(arg)]),
            OP_DUPLICATE_TRANSFER_CAP => answer(reply, &[duplicate_transfer_cap()]),
            OP_EXIT => {
                answer(reply, &[REPLY_OK]);
                thread_exit(arg)
            }
            OP_PROCESS_EXIT => {
                if !start_parked_sibling() {
                    answer(reply, &[REPLY_SPAWN_FAILED]);
                    thread_exit(FAILED_EXIT_CODE)
                }
                answer(reply, &[REPLY_OK]);
                process_exit(arg)
            }
            _ => thread_exit(FAILED_EXIT_CODE),
        }
    }
}

fn start_parked_sibling() -> bool {
    let Ok(ready) = Signal::create() else {
        return false;
    };
    let ready_handle = ready.as_raw();
    let spawned = spawn_owned(move || park_forever(ready_handle));
    if spawned.is_err() {
        return false;
    }
    ready.wait(SIGNALED, PARK_TIMEOUT).is_ok()
}

enum Sibling {
    Recv,
    Call(Port),
    Spin,
}

fn start_sibling(sibling: Sibling) -> u8 {
    let Ok(ready) = Signal::create() else {
        return REPLY_SPAWN_FAILED;
    };
    let ready_handle = ready.as_raw();
    let spawned = spawn_owned(move || occupy(sibling, ready_handle));
    if spawned.is_err() || ready.wait(SIGNALED, PARK_TIMEOUT).is_err() {
        return REPLY_SPAWN_FAILED;
    }

    REPLY_OK
}

fn occupy(sibling: Sibling, ready: Handle<kind::Signal>) -> u32 {
    match sibling {
        Sibling::Recv => {
            let Ok(port) = Port::create() else {
                return 1;
            };
            let _ = signal_set(ready, SIGNALED, 0, WakeCount::All);
            let _ = port.recv(Timeout::INFINITE);
        }
        Sibling::Call(port) => {
            let _ = signal_set(ready, SIGNALED, 0, WakeCount::All);
            ipc_raw::write_frame(&[], &[]);
            let _ = port.call(Timeout::INFINITE);
        }
        Sibling::Spin => {
            let _ = signal_set(ready, SIGNALED, 0, WakeCount::All);
            loop {
                core::hint::spin_loop();
            }
        }
    }
    0
}

fn join_thread() -> u8 {
    let joined = spawn_owned(|| {
        TOUCHED.set(TOUCHED.get() + 1);
        0
    })
    .and_then(|thread| thread.join(PARK_TIMEOUT));

    match joined {
        Ok(()) => REPLY_OK,
        Err(_) => REPLY_SPAWN_FAILED,
    }
}

fn park_forever(ready: Handle<kind::Signal>) -> u32 {
    let Ok(signal) = Signal::create() else {
        return 1;
    };
    let _ = signal_set(ready, SIGNALED, 0, WakeCount::All);
    let _ = signal.wait(SIGNALED, Timeout::INFINITE);
    0
}

fn answer(reply: Reply, body: &[u8]) {
    ipc_raw::write_frame(body, &[]);
    if reply.reply().is_err() {
        thread_exit(FAILED_EXIT_CODE)
    }
}

fn hold_cap(cap: Option<IssuedHandle<kind::Any>>) -> u8 {
    let Some(handle) = cap else {
        return REPLY_HOLD_FAILED;
    };
    let _ = handle.leak();
    REPLY_OK
}

fn hold_self() -> u8 {
    match Process::self_process() {
        Ok(process) => {
            let _ = process.into_raw();
            REPLY_OK
        }
        Err(_) => REPLY_HOLD_FAILED,
    }
}

fn allocate_pages(pages: u64) -> u8 {
    let Ok(resource) = Resource::self_resource() else {
        return REPLY_ALLOCATE_FAILED;
    };
    match resource.create_virtual(pages.saturating_mul(USERLAND_PAGE_SIZE), MemoryAccess::RW) {
        Ok(_) => REPLY_OK,
        Err(Error::Syscall(SyscallError::ResourceExhausted)) => REPLY_BUDGET_EXHAUSTED,
        Err(_) => REPLY_ALLOCATE_FAILED,
    }
}

fn duplicate_transfer_cap() -> u8 {
    let Ok(resource) = Resource::self_resource() else {
        return REPLY_DUPLICATE_FAILED;
    };
    match resource.duplicate(Rights::WRITE | Rights::TRANSFER, None) {
        Ok(_) => REPLY_OK,
        Err(Error::Syscall(SyscallError::AccessDenied)) => REPLY_ACCESS_DENIED,
        Err(_) => REPLY_DUPLICATE_FAILED,
    }
}

fn map_check(cap: Option<IssuedHandle<kind::Any>>, expected: u64) -> u8 {
    let Some(region) = cap else {
        return REPLY_MAP_FAILED;
    };
    let region = MemoryRegion::from(region.assume_kind());
    let Ok(mapping) = Mapping::full(region, UserMemFlags::ReadWrite) else {
        return REPLY_MAP_FAILED;
    };

    let slot = usize::try_from(mapping.va()).expect("positive va fits usize") as *mut u64;
    // SAFETY: регион не меньше страницы, маппинг покрывает его целиком; чтение
    // и запись первых 8 байт лежат в его границах, а другие владельцы отсутствуют.
    let observed = unsafe {
        let observed = slot.read_volatile();
        slot.write_volatile(!expected);
        observed
    };

    if observed == expected {
        REPLY_OK
    } else {
        REPLY_PATTERN_MISMATCH
    }
}
