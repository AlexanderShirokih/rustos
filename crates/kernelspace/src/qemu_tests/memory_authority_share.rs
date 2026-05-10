//! Cross-process sharing `KObject::Memory` через Channel.
//!
//! Producer (process A) получает `MemoryAuthority` + producer-end канала,
//! минтит Virtual-регион через `MemoryCreateVirtual`, маппит, пишет
//! магический паттерн и пересылает handle региона по каналу. Consumer
//! (process B) получает consumer-end + Event, ждёт `CHANNEL_READABLE`,
//! читает handle из канала, маппит регион в свой AS, проверяет паттерн
//! и сигналит Event при совпадении.
//!
//! Сигнал на Event-е == доказательство, что один backing виден из двух
//! разных user-AS как одна и та же страница.

use alloc::vec;

use kobject::{Channel, EVENT_SIGNALED, Event, Handle, KObject, MemoryAuthority, Rights};
use memory::{
    MemFlags,
    aligned::Aligned,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};
use scheduler::{Priority, SchedulerServiceExt, UserProcessLaunch};
use syscall::SyscallOp;
use test_harness_qemu::register_test;
use userspace::{UserImage, UserSegment};

const PAGE_SIZE: usize = PageAlignedVirtualAddress::ALIGNMENT;
const USER_PAYLOAD_VA: usize = 0x4000_0000;
const USER_STACK_TOP: usize = USER_PAYLOAD_VA + 16 * PAGE_SIZE;
const USER_STACK_SIZE: usize = PAGE_SIZE;
/// Скретч-адрес внутри user-стека: туда producer кладёт handle перед
/// `ChannelWrite`, consumer читает handle после `ChannelRead`. Стек
/// замаплен RW, лежит в `[USER_STACK_TOP - USER_STACK_SIZE, USER_STACK_TOP)`.
const SCRATCH_VA: usize = USER_STACK_TOP - 0x100;

const fn svc(op: SyscallOp) -> u32 {
    0xD400_0001 | ((op as u32) << 5)
}

const SVC_MEMORY_CREATE_VIRTUAL: u32 = svc(SyscallOp::MemoryCreateVirtual);
const SVC_MEMORY_MAP: u32 = svc(SyscallOp::MemoryMap);
const SVC_CHANNEL_WRITE: u32 = svc(SyscallOp::ChannelWrite);
const SVC_CHANNEL_READ: u32 = svc(SyscallOp::ChannelRead);
const SVC_OBJECT_SIGNAL: u32 = svc(SyscallOp::ObjectSignal);
const SVC_OBJECT_WAIT_ONE: u32 = svc(SyscallOp::ObjectWaitOne);
const SVC_THREAD_EXIT: u32 = svc(SyscallOp::ThreadExit);

const fn movz(rd: u32, imm: u16, shift: u32) -> u32 {
    0xD280_0000 | (shift << 21) | ((imm as u32) << 5) | rd
}

const fn movk(rd: u32, imm: u16, shift: u32) -> u32 {
    0xF280_0000 | (shift << 21) | ((imm as u32) << 5) | rd
}

const fn mov_xrd_xrm(rd: u32, rm: u32) -> u32 {
    0xAA00_03E0 | (rm << 16) | rd
}

/// `str x{rt}, [x{rn}]` (unsigned offset 0).
const fn str_xrt_xrn(rt: u32, rn: u32) -> u32 {
    0xF900_0000 | (rn << 5) | rt
}

/// `ldr w{rt}, [x{rn}]` (unsigned offset 0, 32-битная загрузка с
/// zero-extend в x).
const fn ldr_wrt_xrn(rt: u32, rn: u32) -> u32 {
    0xB940_0000 | (rn << 5) | rt
}

/// `ldr x{rt}, [x{rn}]` (unsigned offset 0).
const fn ldr_xrt_xrn(rt: u32, rn: u32) -> u32 {
    0xF940_0000 | (rn << 5) | rt
}

/// `str w{rt}, [x{rn}]` (unsigned offset 0).
const fn str_wrt_xrn(rt: u32, rn: u32) -> u32 {
    0xB900_0000 | (rn << 5) | rt
}

const fn cmp_xn_xm(rn: u32, rm: u32) -> u32 {
    0xEB00_001F | (rm << 16) | (rn << 5)
}

/// `b.ne #+disp_words`.
const fn b_ne(disp_words: u32) -> u32 {
    let imm19 = disp_words & 0x7FFFF;
    0x5400_0001 | (imm19 << 5)
}

const B_LOOP: u32 = 0x1400_0000;

const PATTERN_LO: u16 = 0xAB1E;
const PATTERN_MID_LO: u16 = 0xC0DE;
const PATTERN_MID_HI: u16 = 0xF00D;
const PATTERN_HI: u16 = 0x5EED;

/// HandleId-ы для свежей таблицы детерминированы: первый insert -
/// slot=0 generation=1 -> raw = 0x0001_0000. Каждое последующее
/// инкрементирует slot.
const HANDLE_RAW_FIRST: u32 = 0x0001_0000;
const HANDLE_RAW_SECOND: u32 = 0x0001_0001;

/// Загрузка 16-битного immediate в нижние биты `rd`, остальные обнуляются.
fn movz16(words: &mut [u32], i: &mut usize, rd: u32, imm: u16) {
    words[*i] = movz(rd, imm, 0);
    *i += 1;
}

/// Полный 32-битный immediate (всегда movz+movk, фиксированный размер
/// в две инструкции — payload-индексы остаются предсказуемыми).
fn movz_movk32_fixed(words: &mut [u32], i: &mut usize, rd: u32, imm: u32) {
    words[*i] = movz(rd, imm as u16, 0);
    *i += 1;
    words[*i] = movk(rd, (imm >> 16) as u16, 1);
    *i += 1;
}

const PRODUCER_PAYLOAD_WORDS: usize = 32;

/// Producer payload:
/// * x21 = producer_end_handle (raw=0x0001_0000)
/// * x22 = authority_handle (raw=0x0001_0001)
/// * x23 = scratch VA для handle-массива
///
/// Шаги:
///   MemoryCreateVirtual(x22, 0x1000, 3=RW) -> x0 = region_h
///   x19 = region_h
///   MemoryMap(x19, 0x1000, 0=RW) -> x0 = va
///   *va = pattern (8 байт)
///   *scratch = region_h (4 байта u32)
///   ChannelWrite(x21, 0, 0, x23, 1) -> 0
///   ThreadExit(0)
fn build_producer_payload() -> [u8; PRODUCER_PAYLOAD_WORDS * 4] {
    let mut words: [u32; PRODUCER_PAYLOAD_WORDS] = [B_LOOP; PRODUCER_PAYLOAD_WORDS];
    let mut i = 0usize;

    // x21 = producer_end_handle
    movz_movk32_fixed(&mut words, &mut i, 21, HANDLE_RAW_FIRST);
    // x22 = authority_handle
    movz_movk32_fixed(&mut words, &mut i, 22, HANDLE_RAW_SECOND);
    // x23 = SCRATCH_VA
    movz_movk32_fixed(&mut words, &mut i, 23, SCRATCH_VA as u32);

    // MemoryCreateVirtual(x22, 0x1000, RW=3)
    words[i] = mov_xrd_xrm(0, 22);
    i += 1;
    movz16(&mut words, &mut i, 1, 0x1000);
    movz16(&mut words, &mut i, 2, 0x3);
    words[i] = SVC_MEMORY_CREATE_VIRTUAL;
    i += 1;
    // x19 = region_h
    words[i] = mov_xrd_xrm(19, 0);
    i += 1;

    // MemoryMap(x19, 0x1000, 0)
    words[i] = mov_xrd_xrm(0, 19);
    i += 1;
    movz16(&mut words, &mut i, 1, 0x1000);
    movz16(&mut words, &mut i, 2, 0x0);
    words[i] = SVC_MEMORY_MAP;
    i += 1;
    // x20 = va
    words[i] = mov_xrd_xrm(20, 0);
    i += 1;

    // x24 = pattern
    words[i] = movz(24, PATTERN_LO, 0);
    i += 1;
    words[i] = movk(24, PATTERN_MID_LO, 1);
    i += 1;
    words[i] = movk(24, PATTERN_MID_HI, 2);
    i += 1;
    words[i] = movk(24, PATTERN_HI, 3);
    i += 1;
    // *va = pattern
    words[i] = str_xrt_xrn(24, 20);
    i += 1;

    // *scratch = region_h (u32)
    words[i] = str_wrt_xrn(19, 23);
    i += 1;

    // ChannelWrite(x21, 0, 0, x23, 1)
    words[i] = mov_xrd_xrm(0, 21);
    i += 1;
    movz16(&mut words, &mut i, 1, 0);
    movz16(&mut words, &mut i, 2, 0);
    words[i] = mov_xrd_xrm(3, 23);
    i += 1;
    movz16(&mut words, &mut i, 4, 1);
    words[i] = SVC_CHANNEL_WRITE;
    i += 1;

    // ThreadExit(0)
    movz16(&mut words, &mut i, 0, 0);
    words[i] = SVC_THREAD_EXIT;
    i += 1;
    words[i] = B_LOOP;
    i += 1;

    debug_assert!(i <= PRODUCER_PAYLOAD_WORDS, "producer payload overflow");
    let _ = i;

    let mut bytes = [0u8; PRODUCER_PAYLOAD_WORDS * 4];
    for (idx, w) in words.iter().enumerate() {
        bytes[idx * 4..(idx + 1) * 4].copy_from_slice(&w.to_le_bytes());
    }
    bytes
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
    let mut words: [u32; CONSUMER_PAYLOAD_WORDS] = [B_LOOP; CONSUMER_PAYLOAD_WORDS];
    let mut i = 0usize;

    // x21 = consumer_end_handle
    movz_movk32_fixed(&mut words, &mut i, 21, HANDLE_RAW_FIRST);
    // x22 = event_handle
    movz_movk32_fixed(&mut words, &mut i, 22, HANDLE_RAW_SECOND);
    // x23 = SCRATCH_VA
    movz_movk32_fixed(&mut words, &mut i, 23, SCRATCH_VA as u32);

    // ObjectWaitOne(x21, 1, big_timeout=10s).
    // 10s = 10_000_000_000 ns ~= 0x2_540B_E400 - не лезет в 16 бит,
    // используем movz+movk с шагами 16. Положим 0xFFFF_FFFF (~4.29s)
    // как достаточный лимит: тесты должны сработать гораздо раньше.
    // 0xFFFF_FFFF = movz x2, #0xFFFF; movk x2, #0xFFFF, lsl #16.
    words[i] = mov_xrd_xrm(0, 21);
    i += 1;
    movz16(&mut words, &mut i, 1, 1);
    words[i] = movz(2, 0xFFFF, 0);
    i += 1;
    words[i] = movk(2, 0xFFFF, 1);
    i += 1;
    words[i] = SVC_OBJECT_WAIT_ONE;
    i += 1;

    // ChannelRead(x21, 0, 0, x23, 1)
    words[i] = mov_xrd_xrm(0, 21);
    i += 1;
    movz16(&mut words, &mut i, 1, 0);
    movz16(&mut words, &mut i, 2, 0);
    words[i] = mov_xrd_xrm(3, 23);
    i += 1;
    movz16(&mut words, &mut i, 4, 1);
    words[i] = SVC_CHANNEL_READ;
    i += 1;

    // x19 = *scratch (u32 region_h)
    words[i] = ldr_wrt_xrn(19, 23);
    i += 1;

    // MemoryMap(x19, 0x1000, 0)
    words[i] = mov_xrd_xrm(0, 19);
    i += 1;
    movz16(&mut words, &mut i, 1, 0x1000);
    movz16(&mut words, &mut i, 2, 0x0);
    words[i] = SVC_MEMORY_MAP;
    i += 1;
    // x20 = va
    words[i] = mov_xrd_xrm(20, 0);
    i += 1;

    // x24 = *va
    words[i] = ldr_xrt_xrn(24, 20);
    i += 1;
    // x25 = expected pattern
    words[i] = movz(25, PATTERN_LO, 0);
    i += 1;
    words[i] = movk(25, PATTERN_MID_LO, 1);
    i += 1;
    words[i] = movk(25, PATTERN_MID_HI, 2);
    i += 1;
    words[i] = movk(25, PATTERN_HI, 3);
    i += 1;
    // cmp x24, x25
    words[i] = cmp_xn_xm(24, 25);
    i += 1;
    // b.ne -> прыгаем за пределы payload'а в B_LOOP-хвост: смещение
    // считается до индекса последнего слова с B_LOOP. Подсчитаем
    // вручную ниже.
    let bne_index = i;
    i += 1; // зарезервировали место под b.ne

    // ObjectSignal(x22, EVENT_SIGNALED=1, 0)
    words[i] = mov_xrd_xrm(0, 22);
    i += 1;
    movz16(&mut words, &mut i, 1, EVENT_SIGNALED as u16);
    movz16(&mut words, &mut i, 2, 0);
    words[i] = SVC_OBJECT_SIGNAL;
    i += 1;

    // ThreadExit(0)
    movz16(&mut words, &mut i, 0, 0);
    words[i] = SVC_THREAD_EXIT;
    i += 1;
    let fail_index = i;
    words[i] = B_LOOP;
    i += 1;

    let disp = (fail_index - bne_index) as u32;
    words[bne_index] = b_ne(disp);

    debug_assert!(i <= CONSUMER_PAYLOAD_WORDS, "consumer payload overflow");
    let _ = i;

    let mut bytes = [0u8; CONSUMER_PAYLOAD_WORDS * 4];
    for (idx, w) in words.iter().enumerate() {
        bytes[idx * 4..(idx + 1) * 4].copy_from_slice(&w.to_le_bytes());
    }
    bytes
}

fn aligned(va: usize) -> PageAlignedVirtualAddress {
    PageAlignedVirtualAddress::from_usize(va).expect("user VA must be 4K aligned")
}

fn userspace_memory_authority_share_region() {
    let event = Event::new();
    let authority = MemoryAuthority::new();
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
    let authority_ko = KObject::MemoryAuthority(authority);
    let authority_handle = Handle::new(authority_ko.clone(), Rights::defaults_for(&authority_ko));

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
    super::user_process_launcher()
        .spawn_user_process_with_launch(
            "memory-share-consumer",
            &consumer_image,
            Priority::highest(),
            2,
            consumer_launch,
        )
        .expect("consumer spawn must succeed");

    let producer_launch =
        UserProcessLaunch::new().initial_handles(vec![producer_chan_handle, authority_handle]);
    super::user_process_launcher()
        .spawn_user_process_with_launch(
            "memory-share-producer",
            &producer_image,
            Priority::highest(),
            2,
            producer_launch,
        )
        .expect("producer spawn must succeed");

    let scheduler = super::scheduler().clone();
    let mut spins = 0u64;
    while event.peek() & EVENT_SIGNALED == 0 {
        scheduler.sleep_ms(10);
        spins += 1;
        test_harness_qemu::kassert!(spins < 500);
    }

    let _ = spins;
}

register_test!(
    USERSPACE_MEMORY_AUTHORITY_SHARE_REGION,
    "userspace_memory_authority_share_region",
    userspace_memory_authority_share_region
);
