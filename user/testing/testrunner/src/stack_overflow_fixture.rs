//! Программа-фикстура переполнения стека потока. Поток со стеком `THREAD_STACK_BYTES`
//! уходит в рекурсию кадрами с буфером размера из отчёта и пишет адреса своих кадров в отчёт.
//! Процесс завершается отказом доступа к памяти, а собственный код выхода фикстуры называет
//! путь без отказа. Стартовый handle - регион страницы отчёта.
#![no_std]
#![no_main]

use core::{hint::black_box, mem::MaybeUninit, sync::atomic::Ordering::Relaxed};

use kernel_objects::MemoryRegion;
use runtime::{ExitCode, Timeout, UserMemFlags, memory_map, thread::Builder};
use syscall::{PAGE_SIZE, SyscallError};

mod thread_stack_protocol;

use thread_stack_protocol::{
    JOIN_FAILED_EXIT_CODE, LARGE_FRAME_BYTES, MAP_FAILED_EXIT_CODE, RECURSION_RETURNED_EXIT_CODE,
    SMALL_FRAME_BYTES, SPAWN_FAILED_EXIT_CODE, StackReport, THREAD_STACK_BYTES,
    UNKNOWN_FRAME_EXIT_CODE,
};

const JOIN_WAIT: Timeout = Timeout::from_secs(5);

#[program_entry::process]
fn run(report: &MemoryRegion) -> ExitCode {
    let va = memory_map(
        report.as_raw(),
        PAGE_SIZE as u64,
        UserMemFlags::ReadWrite.raw(),
    );
    if SyscallError::from_return(va).is_some() {
        return ExitCode::from(MAP_FAILED_EXIT_CODE);
    }
    let Ok(address) = usize::try_from(va) else {
        return ExitCode::from(MAP_FAILED_EXIT_CODE);
    };

    // SAFETY: `address` - база RW-маппинга в страницу, отчёт лежит в его начале, а маппинг
    // не снимается до конца процесса.
    #[allow(unsafe_code)]
    let report = unsafe { &*(address as *const StackReport) };

    let descend: fn(&StackReport, u64) -> u64 =
        match usize::try_from(report.frame_bytes.load(Relaxed)) {
            Ok(SMALL_FRAME_BYTES) => descend::<SMALL_FRAME_BYTES>,
            Ok(LARGE_FRAME_BYTES) => descend::<LARGE_FRAME_BYTES>,
            _ => return ExitCode::from(UNKNOWN_FRAME_EXIT_CODE),
        };

    let Ok(thread) = Builder::new()
        .stack_bytes(THREAD_STACK_BYTES)
        .spawn_owned(move || overflow(report, descend))
    else {
        return ExitCode::from(SPAWN_FAILED_EXIT_CODE);
    };

    match thread.join(JOIN_WAIT) {
        Ok(()) => ExitCode::from(RECURSION_RETURNED_EXIT_CODE),
        Err(_) => ExitCode::from(JOIN_FAILED_EXIT_CODE),
    }
}

fn overflow(report: &StackReport, descend: fn(&StackReport, u64) -> u64) -> u32 {
    let marker = 0u8;
    let entry = core::ptr::from_ref(black_box(&marker));
    report.entry_frame.store(entry.addr() as u64, Relaxed);

    let depth = descend(report, 0);
    u32::from(black_box(depth) == 0)
}

#[inline(never)]
fn descend<const FRAME_BYTES: usize>(report: &StackReport, depth: u64) -> u64 {
    // Буфер не инициализируется, и адрес кадра попадает в отчёт до первого обращения к памяти
    // кадра. Без проб стека отчёт успевает записать кадр, перешагнувший защитную страницу.
    let mut frame = MaybeUninit::<[u8; FRAME_BYTES]>::uninit();
    report
        .deepest_frame
        .store(frame.as_ptr().addr() as u64, Relaxed);

    if depth == black_box(u64::MAX) {
        return 0;
    }

    // Буфер жив и после вызова, поэтому вызов не становится хвостовым и кадр не освобождается.
    let below = descend::<FRAME_BYTES>(report, depth.wrapping_add(1));
    black_box(&mut frame);
    below
}
