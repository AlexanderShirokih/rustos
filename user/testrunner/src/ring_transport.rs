//! E2E проверка `RingTransport` из EL0: producer и consumer над общим
//! регионом-кольцом (`SpscRing`) в shared memory и одним "data"-сигналом.
//!
//! Главный поток выступает producer'ом, рабочий поток - consumer'ом. Регион
//! выделяется `memory_create_virtual` + `memory_map`, оборачивается двумя
//! значениями `SpscRing` (по одному на конец). Consumer в цикле
//! `wait_readable` + `read_message` выгребает `FRAMES` кадров, producer шлёт
//! те же `FRAMES` (retry на `WouldBlock` с уступкой планировщику). Проверяем,
//! что все кадры получены по порядку.

use collections::SpscRing;
use ipc::{Transport, wire::IpcError};
use kernel_tests::kernel_test;
use runtime::{
    RingTransport, memory_allocate, memory_create_virtual, memory_map, process_resource_self,
    process_self, signal_create, signal_wait_one, thread_create, thread_exit,
};
use syscall::{Handle, MEM_FLAGS_READ_WRITE, SIGNALED};

/// Полезная ёмкость одного слота кольца: вмещает 4-байтовый кадр-счётчик.
const SLOT_PAYLOAD: usize = 4;
/// Число слотов кольца (степень двойки) - намеренно мало, чтобы producer и
/// consumer боролись и проходили путь park/wake.
const RING_CAPACITY: usize = 4;
/// Число кадров, прогоняемых через кольцо.
const FRAMES: u32 = 4096;

/// Размер стека consumer-потока (page-aligned выдача -> вершина выровнена).
const STACK_SIZE: u64 = 0x4000;
/// Таймаут join'а: конечный, чтобы зависший consumer падал по timeout.
const JOIN_TIMEOUT_NS: u64 = 5_000_000_000;
/// Таймаут одного `wait_readable` consumer'а.
const WAIT_TIMEOUT_NS: u64 = 1_000_000_000;

/// `access_mask` региона: биты R|W.
const ACCESS_RW: u64 = 0b11;

/// Размер страницы: регион-кольцо аллоцируется и маппится постранично.
const PAGE_SIZE: u64 = 4096;

/// Разделяемое между потоками состояние теста.
struct Shared {
    /// Базовый VA замапленной страницы-носителя кольца.
    region_va: u64,
    /// Логическая длина кольца в байтах (`region_bytes`), вмещается в страницу.
    region_len: usize,
    /// "data"-сигнал producer->consumer.
    data: Handle,
    /// Признак валидности результата consumer'а (заполняется им перед exit).
    ok: core::sync::atomic::AtomicBool,
}

/// Consumer-поток: оборачивает регион в consumer-`SpscRing`, выгребает все
/// `FRAMES` кадров через `RingTransport` и сверяет их порядок.
extern "C" fn consumer_worker(arg: usize) -> ! {
    // SAFETY: arg - адрес Shared в кадре главного потока; жив до join'а,
    // который выполняется до выхода из его кадра.
    let shared = unsafe { &*(arg as *const Shared) };

    let base = shared.region_va as *mut u8;
    // SAFETY: страница-носитель замаплена RW главным потоком и живёт до конца
    // теста; `region_len <= PAGE_SIZE`, поэтому кольцо лежит в её границах.
    // Этот consumer - единственный consumer-конец над ней (SPSC-инвариант:
    // один producer + один consumer).
    let ring = unsafe { SpscRing::from_raw(base, shared.region_len, SLOT_PAYLOAD) }
        .expect("consumer ring");
    let transport = RingTransport::consumer(ring, shared.data);

    let mut next = 0u32;
    let mut handles: [u32; 0] = [];
    while next < FRAMES {
        match transport.wait_readable(WAIT_TIMEOUT_NS) {
            Ok(()) => {}
            Err(_) => thread_exit(1),
        }
        let mut buf = [0u8; SLOT_PAYLOAD];
        match transport.read_message(&mut buf, &mut handles) {
            Ok(len) => {
                if len.bytes != SLOT_PAYLOAD || u32::from_le_bytes(buf) != next {
                    thread_exit(1);
                }
                next += 1;
            }
            // Гонка: проснулись, но кадр ещё не виден - повторяем.
            Err(IpcError::WouldBlock) => core::hint::spin_loop(),
            Err(_) => thread_exit(1),
        }
    }

    shared.ok.store(true, core::sync::atomic::Ordering::Release);
    thread_exit(0)
}

#[kernel_test]
fn ring_transport_round_trip() {
    // Логическая длина кольца влезает в одну страницу; регион аллоцируем и
    // маппим постранично (ядро оперирует страничными размерами).
    let region_len = SpscRing::region_bytes(RING_CAPACITY, SLOT_PAYLOAD);
    kernel_tests::kassert!(region_len as u64 <= PAGE_SIZE);
    // Регион-носитель кольца минтится под метеринг-бюджет процесса.
    let resource = process_resource_self().expect("metering resource handle");
    let region = memory_create_virtual(resource, PAGE_SIZE, ACCESS_RW).expect("region handle");
    let va = memory_map(region, PAGE_SIZE, MEM_FLAGS_READ_WRITE);
    kernel_tests::kassert!(va > 0);

    let data = signal_create().expect("signal_create");

    let shared = Shared {
        region_va: u64::try_from(va).expect("positive va fits u64"),
        region_len,
        data,
        ok: core::sync::atomic::AtomicBool::new(false),
    };

    // Producer-конец: главный поток оборачивает тот же регион в producer-кольцо.
    let base = shared.region_va as *mut u8;
    // SAFETY: страница только что замаплена RW и живёт до конца теста;
    // `region_len <= PAGE_SIZE`, кольцо в её границах; главный поток -
    // единственный producer-конец над ней.
    let producer_ring =
        unsafe { SpscRing::from_raw(base, region_len, SLOT_PAYLOAD) }.expect("producer ring");
    let producer = RingTransport::producer(producer_ring, data);

    // Спавним consumer-поток в текущем процессе.
    let stack = memory_allocate(resource, STACK_SIZE, MEM_FLAGS_READ_WRITE);
    kernel_tests::kassert!(stack > 0);
    let user_sp = u64::try_from(stack).expect("positive va fits u64") + STACK_SIZE;
    let process = process_self().expect("process_self handle");
    let arg = (&raw const shared) as usize;
    let entry: extern "C" fn(usize) -> ! = consumer_worker;
    let thread = thread_create(process, entry as usize as u64, user_sp, arg as u64, 1)
        .expect("thread_create handle");

    // Producer: шлём FRAMES кадров; на полном кольце (WouldBlock) уступаем.
    let handles: [u32; 0] = [];
    let mut sent = 0u32;
    while sent < FRAMES {
        match producer.write_message(&sent.to_le_bytes(), &handles) {
            Ok(()) => sent += 1,
            Err(IpcError::WouldBlock) => core::hint::spin_loop(),
            Err(_) => kernel_tests::kassert!(false),
        }
    }

    // Join consumer'а с конечным таймаутом (ожидание по thread-handle).
    let observed = signal_wait_one(thread, SIGNALED, JOIN_TIMEOUT_NS);
    kernel_tests::kassert_eq!(observed, i64::from(SIGNALED));

    kernel_tests::kassert!(shared.ok.load(core::sync::atomic::Ordering::Acquire));
}
