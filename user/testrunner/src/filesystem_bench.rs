//! Замеры плоскостей данных ramfs: кадровая плоскость над каждым видом хранилища и прямой
//! доступ через отображённый регион.
//!
//! Прогон идёт под TCG, где нет модели кеша и TLB. Зависимость цены операции от размера
//! региона наблюдаема и переносима, абсолютная пропускная способность - нет.

use alloc::vec;

use filesystem_abi::{AccessMode, OpenFlags, Opened, file_system};
use kernel_objects::{MemoryRegion, Resource};
use kernel_tests::{kernel_test, kreport};
use runtime::{Instant, MemoryAccess, PortTransport, UserMemFlags};
use userland::USERLAND_PAGE_SIZE;

use crate::filesystem::{connect, open, path, ram_session};

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

fn acquire(client: &file_system::Client<PortTransport>) -> MemoryRegion {
    let mut response = client
        .acquire_buffer(BENCH_BUFFER)
        .expect("acquire_buffer call");
    let capability = response.value.expect("buffer granted");
    let cap = response.claim(capability).expect("capability owner");
    MemoryRegion::from_handle(cap.into())
}

fn fill(client: &file_system::Client<PortTransport>, opened: &Opened, size: usize) {
    let written = client
        .write(opened.fd, 0, size as u32)
        .expect("write call")
        .value
        .expect("write ok");
    kernel_tests::kassert_eq!(written.count, size as u32);
}

fn measure_frame_plane(
    client: &file_system::Client<PortTransport>,
    opened: &Opened,
    plane: &str,
    size: usize,
) {
    let rounds = rounds_for(size);
    let len = size as u32;

    let read = nanos_per_op(rounds, || {
        client
            .read(opened.fd, 0, len)
            .expect("read call")
            .value
            .expect("read ok");
    });
    kreport!("fs {plane} read bytes={size} ns_per_op={read}");

    let write = nanos_per_op(rounds, || {
        client
            .write(opened.fd, 0, len)
            .expect("write call")
            .value
            .expect("write ok");
    });
    kreport!("fs {plane} write bytes={size} ns_per_op={write}");
}

#[kernel_test(bench)]
fn bench_call_floor() {
    // Пол кадровой плоскости: вызов без обмена данными через буфер сессии, и под ним - syscall
    // без IPC, разделяющий цену входа в ядро и цену раунда.
    let capability = ram_session();
    let client = connect(&capability);
    let buffer = acquire(&client);

    let inspect = nanos_per_op(rounds_for(PROBE), || {
        buffer.inspect().expect("inspect region");
    });
    kreport!("fs call inspect ns_per_op={inspect}");

    let opened = open(
        &client,
        "/bench-floor",
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );

    let stat = nanos_per_op(rounds_for(PROBE), || {
        client
            .stat(opened.fd)
            .expect("stat call")
            .value
            .expect("stat ok");
    });
    kreport!("fs call stat ns_per_op={stat}");
}

#[kernel_test(bench)]
fn bench_plain_memory() {
    // Опорная строка стенда: во сколько обходятся проходы по обычной памяти, из которых сложены
    // staging-копии сервера.
    for size in SIZES {
        let source = vec![0u8; size];
        let mut target = vec![0u8; size];
        let rounds = rounds_for(size);

        let zeroed = nanos_per_op(rounds, || {
            target.fill(0);
            core::hint::black_box(&target);
        });
        kreport!("mem zero bytes={size} ns_per_op={zeroed}");

        let copied = nanos_per_op(rounds, || {
            target.copy_from_slice(&source);
            core::hint::black_box(&target);
        });
        kreport!("mem copy bytes={size} ns_per_op={copied}");
    }
}

#[kernel_test(bench)]
fn bench_ramfs_heap_plane() {
    let capability = ram_session();
    let client = connect(&capability);
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

#[kernel_test(bench)]
fn bench_ramfs_promoted_plane() {
    let capability = ram_session();
    let client = connect(&capability);
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
        // Ёмкость закрепляется по длине на момент выдачи: файл наполняется до неё заранее.
        fill(&client, &opened, size);
        let mut response = client
            .backing_memory(opened.fd)
            .expect("backing_memory call");
        let capability = response.value.expect("backing granted");
        let backing =
            MemoryRegion::from_handle(response.claim(capability).expect("capability owner").into());
        let mapped = backing
            .map_full(UserMemFlags::ReadWrite)
            .expect("map promoted backing");

        measure_frame_plane(&client, &opened, "promoted", size);

        let mut sink = vec![0; size];
        let direct = nanos_per_op(rounds_for(size), || {
            mapped.copy_out_at(0, &mut sink).expect("direct read");
        });
        kreport!("fs direct read bytes={size} ns_per_op={direct}");
    }
}

#[kernel_test(bench)]
fn bench_ramfs_imported_plane() {
    let capability = ram_session();
    let client = connect(&capability);
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
            .create_backed(path(name), region.into_handle(), size as u64)
            .expect("create_backed call")
            .value
            .expect("create backed file");
        let opened = open(&client, name, AccessMode::ReadWrite, OpenFlags::NONE);
        measure_frame_plane(&client, &opened, "imported", size);
    }
}

#[kernel_test(bench)]
fn bench_ramfs_imported_scales_with_region() {
    let capability = ram_session();
    let client = connect(&capability);
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
            .create_backed(path(name), region.into_handle(), PROBE as u64)
            .expect("create_backed call")
            .value
            .expect("create backed file");
        let opened = open(&client, name, AccessMode::ReadWrite, OpenFlags::NONE);

        let read = nanos_per_op(rounds_for(PROBE), || {
            client
                .read(opened.fd, 0, PROBE as u32)
                .expect("read call")
                .value
                .expect("read ok");
        });
        kreport!("fs imported read bytes={PROBE} region_pages={pages} ns_per_op={read}");
    }
}
