#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::{hint::black_box, panic::PanicInfo, sync::atomic::Ordering::Relaxed};

use runtime::{Priority, Timeout, UserMemFlags, memory_map, process_exit, spawn_owned};
use syscall::{Handle, PAGE_SIZE, SyscallError, kind};

mod thread_stack_protocol;

use thread_stack_protocol::{
    JOIN_FAILED_EXIT_CODE, MAP_FAILED_EXIT_CODE, RECURSION_RETURNED_EXIT_CODE,
    SPAWN_FAILED_EXIT_CODE, StackReport, THREAD_STACK_BYTES,
};

const JOIN_WAIT: Timeout = Timeout::from_secs(5);

/// Программа-фикстура переполнения стека потока. Поток со стеком [`THREAD_STACK_BYTES`]
/// уходит в рекурсию и пишет адреса своих кадров в отчёт. Процесс завершается отказом
/// доступа к памяти, а собственный код выхода фикстуры называет путь без отказа.
/// `report` - регион страницы отчёта, аргумент точки входа.
#[unsafe(no_mangle)]
pub extern "C" fn _start(report: Handle<kind::MemoryRegion>) -> ! {
    let va = memory_map(report, PAGE_SIZE as u64, UserMemFlags::ReadWrite.raw());
    if SyscallError::from_return(va).is_some() {
        exit(MAP_FAILED_EXIT_CODE);
    }
    let Ok(address) = usize::try_from(va) else {
        exit(MAP_FAILED_EXIT_CODE);
    };

    let Ok(thread) = spawn_owned(
        move || overflow(address),
        THREAD_STACK_BYTES,
        Priority::new(1),
    ) else {
        exit(SPAWN_FAILED_EXIT_CODE);
    };

    match thread.join(JOIN_WAIT) {
        Ok(()) => exit(RECURSION_RETURNED_EXIT_CODE),
        Err(_) => exit(JOIN_FAILED_EXIT_CODE),
    }
}

fn exit(code: u32) -> ! {
    process_exit(u64::from(code))
}

fn overflow(address: usize) -> u32 {
    // SAFETY: `address` - база RW-маппинга в страницу, отчёт лежит в его начале, а маппинг
    // не снимается до конца процесса.
    let report = unsafe { &*(address as *const StackReport) };

    let marker = 0u8;
    let entry = core::ptr::from_ref(black_box(&marker));
    report.entry_frame.store(entry.addr() as u64, Relaxed);

    let depth = descend(report, 0);
    u32::from(black_box(depth) == 0)
}

#[inline(never)]
fn descend(report: &StackReport, depth: u64) -> u64 {
    let mut frame = [0u8; 64];
    black_box(&mut frame);
    report
        .deepest_frame
        .store(frame.as_ptr().addr() as u64, Relaxed);

    if depth == black_box(u64::MAX) {
        return 0;
    }

    let below = descend(report, depth.wrapping_add(1));
    below.wrapping_add(u64::from(frame.first().copied().unwrap_or(0)))
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
