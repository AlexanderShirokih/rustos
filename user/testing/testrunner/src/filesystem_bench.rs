//! Замеры плоскостей данных ramfs: кадровая плоскость над каждым видом хранилища и прямой
//! доступ через отображённый регион.
//!
//! Прогон идёт под TCG, где нет модели кеша и TLB. Зависимость цены операции от размера
//! региона наблюдаема и переносима, абсолютная пропускная способность - нет.

use alloc::vec;

use filesystem_abi::{AccessMode, OpenFlags, Opened, file_system};
use kernel_objects::{MemoryRegion, Resource, SessionTransport};
use runtime::{Instant, MemoryAccess, UserMemFlags};
use userland::USERLAND_PAGE_SIZE;

use crate::filesystem::{connect, open, ram_session};

const SIZES: [usize; 3] = [64, 4096, 64 * 1024];

const BENCH_BUFFER: u32 = 64 * 1024;

const PROBE: usize = 64;

const REGION_PAGES: [u64; 2] = [2, 256];

fn rounds_for(size: usize) -> u64 {
    match size {
        0..=1024 => 256,
        1025..=8192 => 64,
        _ => 16,
    }
}

fn nanos_per_op(rounds: u64, mut operation: impl FnMut()) -> u64 {
    let start = Instant::now();
    for _ in 0..rounds {
        operation();
    }
    let elapsed = Instant::now().as_nanos().saturating_sub(start.as_nanos());
    elapsed / rounds
}

fn acquire(client: &file_system::Client<SessionTransport>) -> MemoryRegion {
    let cap = client.acquire_buffer(BENCH_BUFFER).expect("buffer granted");
    MemoryRegion::from(cap)
}

fn fill(client: &file_system::Client<SessionTransport>, opened: &Opened, size: usize) {
    let written = client.write(opened.fd, 0, size as u32).expect("write ok");
    test_harness::check_eq!(written.count, size as u32);
}

fn measure_frame_plane(
    client: &file_system::Client<SessionTransport>,
    opened: &Opened,
    plane: &str,
    size: usize,
) {
    let rounds = rounds_for(size);
    let len = size as u32;

    let read = nanos_per_op(rounds, || {
        client.read(opened.fd, 0, len).expect("read ok");
    });
    test_harness::report!("fs {plane} read bytes={size} ns_per_op={read}");

    let write = nanos_per_op(rounds, || {
        client.write(opened.fd, 0, len).expect("write ok");
    });
    test_harness::report!("fs {plane} write bytes={size} ns_per_op={write}");
}

#[test_harness::test(bench)]
fn bench_call_floor() {
    // Пол кадровой плоскости: вызов без обмена данными через буфер сессии, и под ним - syscall
    // без IPC, разделяющий цену входа в ядро и цену раунда.
    let client = connect(ram_session());
    let buffer = acquire(&client);

    let inspect = nanos_per_op(rounds_for(PROBE), || {
        buffer.inspect().expect("inspect region");
    });
    test_harness::report!("fs call inspect ns_per_op={inspect}");

    let opened = open(
        &client,
        "/bench-floor",
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );

    let stat = nanos_per_op(rounds_for(PROBE), || {
        client.stat(opened.fd).expect("stat ok");
    });
    test_harness::report!("fs call stat ns_per_op={stat}");
}

#[test_harness::test(bench)]
fn bench_plain_memory() {
    // Опорная строка стенда показывает, во сколько обходятся проходы по обычной памяти, из
    // которых сложены staging-копии сервера.
    for size in SIZES {
        let source = vec![0u8; size];
        let mut target = vec![0u8; size];
        let rounds = rounds_for(size);

        let zeroed = nanos_per_op(rounds, || {
            target.fill(0);
            core::hint::black_box(&target);
        });
        test_harness::report!("mem zero bytes={size} ns_per_op={zeroed}");

        let copied = nanos_per_op(rounds, || {
            target.copy_from_slice(&source);
            core::hint::black_box(&target);
        });
        test_harness::report!("mem copy bytes={size} ns_per_op={copied}");
    }
}

#[test_harness::test(bench)]
fn bench_ramfs_heap_plane() {
    let client = connect(ram_session());
    let _buffer = acquire(&client);

    for (name, size) in ["/bench-heap-a", "/bench-heap-b", "/bench-heap-c"]
        .into_iter()
        .zip(SIZES)
    {
        let opened = open(&client, name, AccessMode::ReadWrite, OpenFlags::CREATE);
        fill(&client, &opened, size);
        measure_frame_plane(&client, &opened, "heap", size);
    }
}

#[test_harness::test(bench)]
fn bench_ramfs_promoted_plane() {
    let client = connect(ram_session());
    let _buffer = acquire(&client);

    for (name, size) in [
        "/bench-promoted-a",
        "/bench-promoted-b",
        "/bench-promoted-c",
    ]
    .into_iter()
    .zip(SIZES)
    {
        let opened = open(&client, name, AccessMode::ReadWrite, OpenFlags::CREATE);
        // Ёмкость закрепляется по длине на момент выдачи. Файл наполняется до неё заранее.
        fill(&client, &opened, size);
        let backing =
            MemoryRegion::from(client.backing_memory(opened.fd).expect("backing granted"));
        let mapped = backing
            .map_full(UserMemFlags::ReadWrite)
            .expect("map promoted backing");

        measure_frame_plane(&client, &opened, "promoted", size);

        let mut sink = vec![0; size];
        let direct = nanos_per_op(rounds_for(size), || {
            mapped.copy_out_at(0, &mut sink).expect("direct read");
        });
        test_harness::report!("fs direct read bytes={size} ns_per_op={direct}");
    }
}

#[test_harness::test(bench)]
fn bench_ramfs_imported_plane() {
    let client = connect(ram_session());
    let _buffer = acquire(&client);
    let resource = Resource::self_resource().expect("self resource");

    for (name, size) in [
        "/bench-imported-a",
        "/bench-imported-b",
        "/bench-imported-c",
    ]
    .into_iter()
    .zip(SIZES)
    {
        let bytes = (size as u64).div_ceil(USERLAND_PAGE_SIZE) * USERLAND_PAGE_SIZE;
        let region = resource
            .create_virtual(bytes, MemoryAccess::RW)
            .expect("create imported region");
        client
            .create_backed(name, region, size as u64)
            .expect("create backed file");
        let opened = open(&client, name, AccessMode::ReadWrite, OpenFlags::NONE);
        measure_frame_plane(&client, &opened, "imported", size);
    }
}

#[test_harness::test(bench)]
fn bench_ramfs_imported_scales_with_region() {
    let client = connect(ram_session());
    let _buffer = acquire(&client);
    let resource = Resource::self_resource().expect("self resource");

    // Операция одна и та же, меняется только размер принесённого региона.
    for (name, pages) in ["/bench-region-small", "/bench-region-large"]
        .into_iter()
        .zip(REGION_PAGES)
    {
        let region = resource
            .create_virtual(pages * USERLAND_PAGE_SIZE, MemoryAccess::RW)
            .expect("create imported region");
        client
            .create_backed(name, region, PROBE as u64)
            .expect("create backed file");
        let opened = open(&client, name, AccessMode::ReadWrite, OpenFlags::NONE);

        let read = nanos_per_op(rounds_for(PROBE), || {
            client.read(opened.fd, 0, PROBE as u32).expect("read ok");
        });
        test_harness::report!(
            "fs imported read bytes={PROBE} region_pages={pages} ns_per_op={read}"
        );
    }
}
