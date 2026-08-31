//! E2E проверка `RingTransport` из EL0: producer и consumer над общим
//! регионом-кольцом (`SpscRing`) в shared memory и одним "data"-сигналом.
//!
//! Главный поток выступает стороной producer, рабочий поток - стороной
//! consumer. Регион создаётся через `Resource`/`MemoryRegion` и маппится,
//! оборачивается двумя значениями `SpscRing` (по одному на конец). Consumer в
//! цикле `wait_readable` + `read_message` выгребает `FRAMES` кадров, producer
//! шлёт те же `FRAMES` (retry на `WouldBlock` с уступкой планировщику).
//! Проверяем, что все кадры получены по порядку.

use collections::SpscRing;
use ipc::{Transport, wire::IpcError};
use kernel_objects::{Resource, RingTransport, Signal};
use kernel_tests::kernel_test;
use runtime::{MemoryAccess, Priority, Timeout, UserMemFlags, spawn};
use syscall::Handle;

const SLOT_PAYLOAD: usize = 4;
const RING_CAPACITY: usize = 4;
const FRAMES: u32 = 4096;

const STACK_SIZE: u64 = 0x4000;
const JOIN_TIMEOUT: Timeout = Timeout::from_secs(5);
const WAIT_TIMEOUT: Timeout = Timeout::from_secs(1);

const PAGE_SIZE: u64 = syscall::PAGE_SIZE as u64;

struct Shared {
    region_va: u64,
    region_len: usize,
    data: Handle,
    ok: core::sync::atomic::AtomicBool,
}

extern "C" fn consumer_worker(arg: usize) -> u32 {
    // SAFETY: arg - адрес Shared в кадре главного потока; жив до завершения
    // join, который выполняется до выхода из его кадра.
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
        match transport.wait_readable(WAIT_TIMEOUT) {
            Ok(()) => {}
            Err(_) => return 1,
        }
        let mut buf = [0u8; SLOT_PAYLOAD];
        match transport.read_message(&mut buf, &mut handles) {
            Ok(len) => {
                if len.bytes != SLOT_PAYLOAD || u32::from_le_bytes(buf) != next {
                    return 1;
                }
                next += 1;
            }
            // Гонка: проснулись, но кадр ещё не виден - повторяем.
            Err(IpcError::WouldBlock) => core::hint::spin_loop(),
            Err(_) => return 1,
        }
    }

    shared.ok.store(true, core::sync::atomic::Ordering::Release);
    0
}

#[kernel_test]
fn ring_transport_round_trip() {
    // Логическая длина кольца влезает в одну страницу; регион аллоцируем и
    // маппим постранично (ядро оперирует страничными размерами).
    let region_len = SpscRing::region_bytes(RING_CAPACITY, SLOT_PAYLOAD);
    kernel_tests::kassert!(region_len as u64 <= PAGE_SIZE);
    // Регион-носитель кольца минтится под метеринг-бюджет процесса.
    let resource = Resource::self_resource().expect("self metering resource");
    let region = resource
        .create_virtual(PAGE_SIZE, MemoryAccess::RW)
        .expect("region");
    let mapping = region
        .map(PAGE_SIZE, UserMemFlags::ReadWrite)
        .expect("map region");
    kernel_tests::kassert!(mapping.va() > 0);

    let data_signal = Signal::create().expect("signal create");

    let shared = Shared {
        region_va: mapping.va(),
        region_len,
        data: data_signal.handle().as_raw(),
        ok: core::sync::atomic::AtomicBool::new(false),
    };

    // Producer-конец: главный поток оборачивает тот же регион в producer-кольцо.
    let base = shared.region_va as *mut u8;
    // SAFETY: страница только что замаплена RW и живёт до конца теста;
    // `region_len <= PAGE_SIZE`, кольцо в её границах; главный поток -
    // единственный producer-конец над ней.
    let producer_ring =
        unsafe { SpscRing::from_raw(base, region_len, SLOT_PAYLOAD) }.expect("producer ring");
    let producer = RingTransport::producer_signal(producer_ring, data_signal.handle());

    // Спавним consumer-поток в текущем процессе.
    let arg = (&raw const shared) as usize;
    let thread =
        spawn(consumer_worker, arg, STACK_SIZE, Priority::new(1)).expect("spawn consumer worker");

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

    // Ожидание consumer с конечным таймаутом (по thread-handle).
    thread
        .join(JOIN_TIMEOUT)
        .expect("consumer joins before timeout");

    kernel_tests::kassert!(shared.ok.load(core::sync::atomic::Ordering::Acquire));
}
