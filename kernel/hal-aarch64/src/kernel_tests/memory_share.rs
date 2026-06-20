//! Разделяемая память между двумя адресными пространствами через перенос memory-handle по IPC.

extern crate alloc;

use alloc::sync::Arc;
use core::{
    num::NonZeroUsize,
    sync::atomic::{AtomicU64, Ordering},
};

use collections::{LockCell, MutexCell};
use kernel_tests::kernel_test;
use kernelspace::syscall_bridge;
use kobject::{
    Handle, HandleId, HandleTable, KObject, KernelIpcBuffer, Rights, ThreadTransport,
    transfer_rendezvous,
};
use memory::{AccessMask, MemFlags, MemoryRegion, virtual_address::PageAlignedVirtualAddress};
use scheduler::AddressSpace;
use syscall::{IpcBuffer, encode_tag};

/// Lower-half VA в AS A, не пересекающаяся с другими user-AS-тестами.
const VA_A: usize = 0x5100_0000;
/// Намеренно ДРУГАЯ VA в AS B: общий не VA, а физический фрейм.
const VA_B: usize = 0x6100_0000;

/// 8-байтный паттерн, который producer пишет через mapper A, а consumer
/// читает через mapper B.
const PATTERN: u64 = 0x5EED_F00D_C0DE_AB1E;

/// Удерживает Arc на user-AS и регион живыми до конца QEMU-runner'а:
/// освобождение фреймов между тестами привело бы к их повторной выдаче и
/// рассинхронизации с TLB/бэкингом.
static HOLD_AS_A: AtomicU64 = AtomicU64::new(0);
static HOLD_AS_B: AtomicU64 = AtomicU64::new(0);
static HOLD_REGION: AtomicU64 = AtomicU64::new(0);

fn make_user_as() -> Arc<AddressSpace> {
    let factory =
        syscall_bridge::address_space_factory().expect("address space factory must be installed");
    AddressSpace::new_user(factory).expect("create user AS")
}

fn aligned(va: usize) -> PageAlignedVirtualAddress {
    PageAlignedVirtualAddress::from_usize(va).expect("user VA must be 4K aligned")
}

/// Kernel-транспорт поверх свежего kernel-резидентного IPC-буфера и
/// переданной handle-таблицы - тот же `new_kernel`, что у bootstrap-логгера.
fn kernel_transport(table: Arc<MutexCell<HandleTable>>) -> (ThreadTransport, KernelIpcBuffer) {
    let buffer: KernelIpcBuffer = Arc::new(MutexCell::new(IpcBuffer::zeroed()));
    let transport = ThreadTransport::new_kernel(buffer.clone(), table);
    (transport, buffer)
}

#[kernel_test]
fn memory_region_shared_across_two_address_spaces() {
    // 1. Два независимых user-AS со своими mapper'ами.
    let as_a = make_user_as();
    let as_b = make_user_as();
    let mapper_a = as_a.mapper().expect("user AS A has mapper");
    let mapper_b = as_b.mapper().expect("user AS B has mapper");

    // 2. Анонимный RW-регион + handle на него в table A с TRANSFER.
    let fa = syscall_bridge::frame_allocator().expect("FrameAllocator must be installed");
    let region = MemoryRegion::create_virtual(fa, NonZeroUsize::new(1).unwrap(), AccessMask::RW)
        .expect("create_virtual one page");
    let region = Arc::new(region);
    let ko = KObject::Memory(region.clone());
    let rights = Rights::defaults_for(&ko);
    kernel_tests::kassert!(rights.contains(Rights::TRANSFER));

    let table_a = Arc::new(MutexCell::new(HandleTable::new()));
    let table_b = Arc::new(MutexCell::new(HandleTable::new()));

    let handle_a = table_a
        .with_lock(|tbl| tbl.insert(Handle::new(ko, rights)))
        .expect("insert memory handle into table A");

    // 3. Маппим регион в AS A и пишем паттерн через mapper A.
    region
        .install(mapper_a, aligned(VA_A), MemFlags::user_rw())
        .expect("install region into AS A");
    mapper_a
        .copy_user_out(
            memory::virtual_address::VirtualAddress::new(VA_A),
            &PATTERN.to_le_bytes(),
        )
        .expect("write pattern via mapper A");

    // 4. Перенос memory-handle table A -> table B по port-пути.
    //    Sender пишет caps[0]=handle_a, tag=(len=0, ncaps=1) в свой kernel-буфер.
    let (sender, sender_buf) = kernel_transport(table_a.clone());
    let (receiver, receiver_buf) = kernel_transport(table_b.clone());
    sender_buf.with_lock(|buf| {
        buf.caps[0] = handle_a.raw().get();
        buf.tag = encode_tag(0, 1);
    });

    transfer_rendezvous(&sender, &receiver).expect("memory handle transfer A->B");

    // 5. Достаём перенесённый handle из caps[0] receiver-буфера, мапим ТОТ ЖЕ
    //    регион в AS B на другой VA и читаем через mapper B.
    let raw_b = receiver_buf.with_lock(|buf| buf.caps[0]);
    let id_b = HandleId::from_raw(core::num::NonZeroU32::new(raw_b).expect("non-zero handle id"));
    let region_b = table_b
        .with_lock(|tbl| tbl.get_memory(id_b, Rights::READ | Rights::WRITE))
        .expect("table B holds transferred memory handle");

    region_b
        .install(mapper_b, aligned(VA_B), MemFlags::user_rw())
        .expect("install region into AS B");

    let mut read_back = [0u8; 8];
    mapper_b
        .copy_user_in(
            memory::virtual_address::VirtualAddress::new(VA_B),
            &mut read_back,
        )
        .expect("read pattern via mapper B");

    // Главный assert: байты, записанные через mapper A на VA_A, видны через
    // mapper B на VA_B - общий физический фрейм.
    kernel_tests::kassert_eq!(u64::from_le_bytes(read_back), PATTERN);

    // 6. Move-семантика: в table A handle'а больше нет.
    kernel_tests::kassert!(table_a.with_lock(|tbl| tbl.live_count()) == 0);
    kernel_tests::kassert!(table_b.with_lock(|tbl| tbl.live_count()) == 1);

    // Утечка AS и региона: фреймы не должны вернуться в аллокатор до конца
    // суиты (см. address_space.rs).
    HOLD_AS_A.store(Arc::into_raw(as_a) as usize as u64, Ordering::Release);
    HOLD_AS_B.store(Arc::into_raw(as_b) as usize as u64, Ordering::Release);
    HOLD_REGION.store(Arc::into_raw(region) as usize as u64, Ordering::Release);
}
