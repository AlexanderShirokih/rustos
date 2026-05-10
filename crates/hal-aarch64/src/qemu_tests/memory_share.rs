//! Cross-process sharing `KObject::Memory` через Channel.
//!
//! Producer (process A) минтит Virtual-регион через `MemoryCreateVirtual`,
//! маппит, пишет магический паттерн и пересылает handle региона по каналу.
//! Consumer (process B) получает consumer-end + Event, ждёт
//! `CHANNEL_READABLE`, читает handle из канала, маппит регион в свой AS,
//! проверяет паттерн и сигналит Event при совпадении.
//!
//! Сигнал на Event-е == доказательство, что один backing виден из двух
//! разных user-AS как одна и та же страница.

use alloc::vec;

use kobject::{Channel, EVENT_SIGNALED, Event, Handle, KObject, Rights};
use memory::{
    MemFlags,
    aligned::Aligned,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};
use scheduler::{Priority, SchedulerServiceExt, UserProcessLaunch};
use syscall::SyscallOp;
use test_harness_qemu::register_test;
use userspace::{UserImage, UserSegment};

use super::user_payload::{
    B_LOOP, Builder, Reg, b_ne, cmp_x, ldr_w, ldr_x, mov_x, str_w, str_x, svc_op,
};

const PAGE_SIZE: usize = PageAlignedVirtualAddress::ALIGNMENT;
const USER_PAYLOAD_VA: usize = 0x4000_0000;
const USER_STACK_TOP: usize = USER_PAYLOAD_VA + 16 * PAGE_SIZE;
const USER_STACK_SIZE: usize = PAGE_SIZE;
/// Скретч-адрес внутри user-стека: туда producer кладёт handle перед
/// `ChannelWrite`, consumer читает handle после `ChannelRead`. Стек
/// замаплен RW, лежит в `[USER_STACK_TOP - USER_STACK_SIZE, USER_STACK_TOP)`.
const SCRATCH_VA: usize = USER_STACK_TOP - 0x100;

const PATTERN_LO: u16 = 0xAB1E;
const PATTERN_MID_LO: u16 = 0xC0DE;
const PATTERN_MID_HI: u16 = 0xF00D;
const PATTERN_HI: u16 = 0x5EED;
const PATTERN: u64 = ((PATTERN_HI as u64) << 48)
    | ((PATTERN_MID_HI as u64) << 32)
    | ((PATTERN_MID_LO as u64) << 16)
    | (PATTERN_LO as u64);

/// HandleId-ы для свежей таблицы детерминированы: первый insert -
/// slot=0 generation=1 -> raw = 0x0001_0000. Каждое последующее
/// инкрементирует slot.
const HANDLE_RAW_FIRST: u32 = 0x0001_0000;
const HANDLE_RAW_SECOND: u32 = 0x0001_0001;

const PRODUCER_PAYLOAD_WORDS: usize = 32;

/// Producer payload:
/// * x21 = producer_end_handle (raw=0x0001_0000)
/// * x22 = scratch VA для handle-массива
///
/// Шаги:
///   MemoryCreateVirtual(0x1000, 3=RW) -> x0 = region_h
///   x19 = region_h
///   MemoryMap(x19, 0x1000, 0=RW) -> x0 = va
///   *va = pattern (8 байт)
///   *scratch = region_h (4 байта u32)
///   ChannelWrite(x21, 0, 0, x22, 1) -> 0
///   ThreadExit(0)
fn build_producer_payload() -> [u8; PRODUCER_PAYLOAD_WORDS * 4] {
    let mut payload = Builder::<PRODUCER_PAYLOAD_WORDS>::new(B_LOOP);

    // x21 = producer_end_handle
    payload.mov_u32_fixed(Reg::X21, HANDLE_RAW_FIRST);
    // x22 = SCRATCH_VA
    payload.mov_u32_fixed(Reg::X22, SCRATCH_VA as u32);

    // MemoryCreateVirtual(0x1000, RW=3)
    payload.mov_u16(Reg::X0, 0x1000);
    payload.mov_u16(Reg::X1, 0x3);
    payload.push(svc_op(SyscallOp::MemoryCreateVirtual));
    // x19 = region_h
    payload.push(mov_x(Reg::X19, Reg::X0));

    // MemoryMap(x19, 0x1000, 0)
    payload.push(mov_x(Reg::X0, Reg::X19));
    payload.mov_u16(Reg::X1, 0x1000);
    payload.mov_u16(Reg::X2, 0);
    payload.push(svc_op(SyscallOp::MemoryMap));
    // x20 = va
    payload.push(mov_x(Reg::X20, Reg::X0));

    // x24 = pattern
    payload.mov_u64_fixed(Reg::X24, PATTERN);
    // *va = pattern
    payload.push(str_x(Reg::X24, Reg::X20));

    // *scratch = region_h (u32)
    payload.push(str_w(Reg::X19, Reg::X22));

    // ChannelWrite(x21, 0, 0, x22, 1)
    payload.push(mov_x(Reg::X0, Reg::X21));
    payload.mov_u16(Reg::X1, 0);
    payload.mov_u16(Reg::X2, 0);
    payload.push(mov_x(Reg::X3, Reg::X22));
    payload.mov_u16(Reg::X4, 1);
    payload.push(svc_op(SyscallOp::ChannelWrite));

    // ThreadExit(0)
    payload.mov_u16(Reg::X0, 0);
    payload.push(svc_op(SyscallOp::ThreadExit));
    payload.push(B_LOOP);

    payload.into_bytes()
}

const CONSUMER_PAYLOAD_WORDS: usize = 48;

/// Consumer payload:
/// * x21 = consumer_end_handle (raw=0x0001_0000)
/// * x22 = event_handle (raw=0x0001_0001)
/// * x23 = scratch VA
///
/// Шаги:
///   ObjectWaitOne(x21, CHANNEL_READABLE=1, max_timeout)
///   ChannelRead(x21, 0, 0, x23, 1) -> bytes_len|handles_count
///   region_h = *scratch (u32 zero-ext)
///   MemoryMap(region_h, 0x1000, 0=RW) -> x0 = va
///   x24 = *va  (8 байт)
///   x25 = expected pattern
///   if x24 != x25 -> в b_loop -> timeout-fail
///   ObjectSignal(x22, EVENT_SIGNALED=1, 0)
///   ThreadExit(0)
fn build_consumer_payload() -> [u8; CONSUMER_PAYLOAD_WORDS * 4] {
    let mut payload = Builder::<CONSUMER_PAYLOAD_WORDS>::new(B_LOOP);

    // x21 = consumer_end_handle
    payload.mov_u32_fixed(Reg::X21, HANDLE_RAW_FIRST);
    // x22 = event_handle
    payload.mov_u32_fixed(Reg::X22, HANDLE_RAW_SECOND);
    // x23 = SCRATCH_VA
    payload.mov_u32_fixed(Reg::X23, SCRATCH_VA as u32);

    // ObjectWaitOne(x21, 1, big_timeout=10s).
    // 10s = 10_000_000_000 ns ~= 0x2_540B_E400 - не лезет в 16 бит,
    // используем movz+movk с шагами 16. Положим 0xFFFF_FFFF (~4.29s)
    // как достаточный лимит: тесты должны сработать гораздо раньше.
    payload.push(mov_x(Reg::X0, Reg::X21));
    payload.mov_u16(Reg::X1, 1);
    payload.mov_u32_fixed(Reg::X2, 0xFFFF_FFFF);
    payload.push(svc_op(SyscallOp::ObjectWaitOne));

    // ChannelRead(x21, 0, 0, x23, 1)
    payload.push(mov_x(Reg::X0, Reg::X21));
    payload.mov_u16(Reg::X1, 0);
    payload.mov_u16(Reg::X2, 0);
    payload.push(mov_x(Reg::X3, Reg::X23));
    payload.mov_u16(Reg::X4, 1);
    payload.push(svc_op(SyscallOp::ChannelRead));

    // x19 = *scratch (u32 region_h)
    payload.push(ldr_w(Reg::X19, Reg::X23));

    // MemoryMap(x19, 0x1000, 0)
    payload.push(mov_x(Reg::X0, Reg::X19));
    payload.mov_u16(Reg::X1, 0x1000);
    payload.mov_u16(Reg::X2, 0);
    payload.push(svc_op(SyscallOp::MemoryMap));
    // x20 = va
    payload.push(mov_x(Reg::X20, Reg::X0));

    // x24 = *va
    payload.push(ldr_x(Reg::X24, Reg::X20));
    // x25 = expected pattern
    payload.mov_u64_fixed(Reg::X25, PATTERN);
    // cmp x24, x25
    payload.push(cmp_x(Reg::X24, Reg::X25));
    // b.ne -> прыгаем за пределы payload'а в B_LOOP-хвост: смещение
    // считается до индекса последнего слова с B_LOOP. Подсчитаем
    // вручную ниже.
    let bne_index = payload.reserve(B_LOOP);

    // ObjectSignal(x22, EVENT_SIGNALED=1, 0)
    payload.push(mov_x(Reg::X0, Reg::X22));
    payload.mov_u16(Reg::X1, EVENT_SIGNALED as u16);
    payload.mov_u16(Reg::X2, 0);
    payload.push(svc_op(SyscallOp::ObjectSignal));

    // ThreadExit(0)
    payload.mov_u16(Reg::X0, 0);
    payload.push(svc_op(SyscallOp::ThreadExit));
    let fail_index = payload.len();
    payload.push(B_LOOP);

    let disp = (fail_index - bne_index) as u32;
    payload.set(bne_index, b_ne(disp));

    payload.into_bytes()
}

fn aligned(va: usize) -> PageAlignedVirtualAddress {
    PageAlignedVirtualAddress::from_usize(va).expect("user VA must be 4K aligned")
}

fn userspace_memory_share_region() {
    let event = Event::new();
    let (producer_end, consumer_end) = Channel::create_pair(0);

    let producer_payload = build_producer_payload();
    let consumer_payload = build_consumer_payload();

    let producer_segment = UserSegment {
        va_base: aligned(USER_PAYLOAD_VA),
        mapped_size: PAGE_SIZE,
        init_bytes: &producer_payload,
        perms: MemFlags::user_rx(),
    };
    let producer_image = UserImage {
        segments: core::slice::from_ref(&producer_segment),
        entry: VirtualAddress::new(USER_PAYLOAD_VA),
        user_stack_top: VirtualAddress::new(USER_STACK_TOP),
        user_stack_size: USER_STACK_SIZE,
    };

    let consumer_segment = UserSegment {
        va_base: aligned(USER_PAYLOAD_VA),
        mapped_size: PAGE_SIZE,
        init_bytes: &consumer_payload,
        perms: MemFlags::user_rx(),
    };
    let consumer_image = UserImage {
        segments: core::slice::from_ref(&consumer_segment),
        entry: VirtualAddress::new(USER_PAYLOAD_VA),
        user_stack_top: VirtualAddress::new(USER_STACK_TOP),
        user_stack_size: USER_STACK_SIZE,
    };

    let producer_chan_ko = KObject::Channel(producer_end);
    let producer_chan_handle = Handle::new(
        producer_chan_ko.clone(),
        Rights::defaults_for(&producer_chan_ko),
    );

    let consumer_chan_ko = KObject::Channel(consumer_end);
    let consumer_chan_handle = Handle::new(
        consumer_chan_ko.clone(),
        Rights::defaults_for(&consumer_chan_ko),
    );
    let event_handle = Handle::new(KObject::Event(event.clone()), Rights::SIGNAL);

    // Сначала consumer (чтобы он успел запарковаться на ObjectWaitOne к
    // моменту, когда producer выполнит ChannelWrite). Порядок не критичен -
    // канал буферизует, и любая последовательность приведёт к успеху, -
    // но так путь короче.
    let consumer_launch =
        UserProcessLaunch::new().initial_handles(vec![consumer_chan_handle, event_handle]);
    kernelspace::qemu_tests::user_process_launcher()
        .spawn_user_process_with_launch(
            "memory-share-consumer",
            &consumer_image,
            Priority::highest(),
            2,
            consumer_launch,
        )
        .expect("consumer spawn must succeed");

    let producer_launch = UserProcessLaunch::new().initial_handles(vec![producer_chan_handle]);
    kernelspace::qemu_tests::user_process_launcher()
        .spawn_user_process_with_launch(
            "memory-share-producer",
            &producer_image,
            Priority::highest(),
            2,
            producer_launch,
        )
        .expect("producer spawn must succeed");

    let scheduler = kernelspace::qemu_tests::scheduler().clone();
    let mut spins = 0u64;
    while event.peek() & EVENT_SIGNALED == 0 {
        scheduler.sleep_ms(10);
        spins += 1;
        test_harness_qemu::kassert!(spins < 500);
    }

    let _ = spins;
}

register_test!(
    USERSPACE_MEMORY_SHARE_REGION,
    "userspace_memory_share_region",
    userspace_memory_share_region
);
