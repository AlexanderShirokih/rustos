//! Программа-фикстура с портовым протоколом: обслуживает запросы родителя
//! в собственном адресном пространстве.
//!
//! Нужна тестам, которым мало одного exit-кода потомка: изоляции адресных
//! пространств (слот в `.bss` лежит на одном и том же VA у всех экземпляров
//! этого бинаря) и разделяемой памяти (регион приезжает capability'ю в
//! `caps[0]`).

#![no_std]
#![no_main]
#![allow(unsafe_code)]

use core::{
    panic::PanicInfo,
    sync::atomic::{AtomicU64, Ordering},
};

use fixture_protocol::{
    OP_ALLOCATE_PAGES, OP_DUPLICATE_TRANSFER_CAP, OP_EXIT, OP_MAP_CHECK, OP_READ_SLOT,
    OP_WRITE_SLOT, READ_REPLY_LEN, REPLY_ACCESS_DENIED, REPLY_ALLOCATE_FAILED,
    REPLY_BUDGET_EXHAUSTED, REPLY_DUPLICATE_FAILED, REPLY_MAP_FAILED, REPLY_OK,
    REPLY_PATTERN_MISMATCH, REQUEST_LEN, decode_request,
};
use runtime::{
    Error, MemoryAccess, MemoryRegion, OwnedHandle, Port, Reply, Resource, Timeout, UserMemFlags,
    thread_exit,
};
use syscall::{Handle, Rights, SyscallError};
use userland::USERLAND_PAGE_SIZE;

mod fixture_protocol;
mod ipc_raw;

/// Слот в `.bss`: у всех экземпляров этого бинаря он лежит по одному VA,
/// поэтому различие содержимого доказывает изоляцию адресных пространств.
static SLOT: AtomicU64 = AtomicU64::new(0);

/// Кадр не разобран.
const EXIT_BAD_REQUEST: u64 = 101;
/// `recv` на bootstrap-порту отказал не уходом значка либо принёс кадр без
/// reply-хэндла (встречный сделал `send` вместо `call`).
const EXIT_RECV_FAILED: u64 = 102;
/// `reply` не доставлен.
const EXIT_REPLY_FAILED: u64 = 103;

#[unsafe(no_mangle)]
pub extern "C" fn _start(root: Handle) -> ! {
    let port = Port::from_handle(OwnedHandle::from_handle(root));

    loop {
        let reply = match port.recv(Timeout::INFINITE) {
            Ok(Some(reply)) => reply,
            // Значок сессии ушёл: родитель закрыл клиентскую капу либо умер.
            Err(Error::Syscall(SyscallError::SenderGone)) => thread_exit(0),
            _ => thread_exit(EXIT_RECV_FAILED),
        };

        let mut body = [0u8; REQUEST_LEN];
        let (len, ncaps) = ipc_raw::read_frame(&mut body);
        if len < REQUEST_LEN {
            thread_exit(EXIT_BAD_REQUEST)
        }
        let Some((op, arg)) = decode_request(&body) else {
            thread_exit(EXIT_BAD_REQUEST)
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
                let cap = if ncaps == 1 { ipc_raw::read_cap(0) } else { 0 };
                answer(reply, &[map_check(cap, arg)]);
            }
            OP_ALLOCATE_PAGES => answer(reply, &[allocate_pages(arg)]),
            OP_DUPLICATE_TRANSFER_CAP => answer(reply, &[duplicate_transfer_cap()]),
            OP_EXIT => {
                answer(reply, &[REPLY_OK]);
                thread_exit(arg)
            }
            _ => thread_exit(EXIT_BAD_REQUEST),
        }
    }
}

/// Доставляет ответ; недоставленный ответ оставил бы родителя в `call`
/// навсегда, поэтому это терминальная ошибка фикстуры.
fn answer(reply: Reply, body: &[u8]) {
    ipc_raw::write_frame(body, &[]);
    if reply.reply().is_err() {
        thread_exit(EXIT_REPLY_FAILED)
    }
}

// Регион сразу закрывается: наблюдаемо только то, пропустил ли выделение бюджет.
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

// Ширину прав капы метеринг-ресурса задаёт грант старта, у самой фикстуры её нет.
fn duplicate_transfer_cap() -> u8 {
    let Ok(resource) = Resource::self_resource() else {
        return REPLY_DUPLICATE_FAILED;
    };
    match resource
        .handle()
        .duplicate(Rights::WRITE | Rights::TRANSFER, 0)
    {
        Ok(_) => REPLY_OK,
        Err(Error::Syscall(SyscallError::AccessDenied)) => REPLY_ACCESS_DENIED,
        Err(_) => REPLY_DUPLICATE_FAILED,
    }
}

/// Мапит доставленный регион, сверяет первые 8 байт с `expected` и пишет
/// туда инверсию - родитель проверит её через свой маппинг того же фрейма.
fn map_check(cap: u32, expected: u64) -> u8 {
    let Some(handle) = OwnedHandle::from_raw(cap) else {
        return REPLY_MAP_FAILED;
    };
    let region = MemoryRegion::from_handle(handle);
    // Размер берётся из самого региона: договорённость о нём с родителем была
    // бы вторым источником истины, а ядро требует точного совпадения.
    let Ok(mapping) = region.map_full(UserMemFlags::ReadWrite) else {
        return REPLY_MAP_FAILED;
    };

    let slot = usize::try_from(mapping.va()).expect("positive va fits usize") as *mut u64;
    // SAFETY: регион не меньше страницы, маппинг покрывает его целиком; чтение
    // и запись первых 8 байт лежат в его границах, других владельцев нет.
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

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
