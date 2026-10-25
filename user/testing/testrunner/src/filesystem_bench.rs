//! Замеры каноничного файлового API и опорной цены копирования. Прогон под TCG не моделирует
//! кеши и TLB процессора. Результат относится к этому гостевому окружению.

use alloc::{format, vec};

use filesystem_abi::{AccessMode, OpenFlags, Path};
use filesystem_client::FileSystem;
use kernel_objects::{Mapping, Resource, Session};
use runtime::{Instant, MemoryAccess, UserMemFlags};
use userland::USERLAND_PAGE_SIZE;

use crate::filesystem::{connect, open, ram_session, root_view};

const SIZES: [usize; 3] = [64, 4096, 64 * 1024];

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
    Instant::now().as_nanos().saturating_sub(start.as_nanos()) / rounds
}

#[test_harness::test(bench)]
fn bench_call_floor() {
    let client = connect(ram_session());
    let region = Resource::self_resource()
        .expect("resource")
        .create_virtual(USERLAND_PAGE_SIZE, MemoryAccess::RW)
        .expect("region");
    let inspect = nanos_per_op(256, || {
        region.inspect().expect("inspect");
    });
    let opened = open(
        &client,
        "/bench-floor",
        AccessMode::ReadWrite,
        OpenFlags::CREATE,
    );
    let stat = nanos_per_op(256, || {
        client.stat(opened.fd).wait().expect("stat");
    });
    test_harness::report!("fs control inspect ns_per_op={inspect} stat_ns_per_op={stat}");
}

#[test_harness::test(bench)]
fn bench_ramfs_canonical_io() {
    let mut filesystem = FileSystem::new(Session::from_handle(root_view()));
    for size in SIZES {
        let name = format!("/tmp/bench-api-{size}");
        let mut node = filesystem
            .open(
                Path::new(&name).expect("path"),
                AccessMode::ReadWrite,
                OpenFlags::CREATE | OpenFlags::TRUNCATE,
            )
            .expect("open");
        let source = vec![0x5A; size];
        let mut target = vec![0xA5; size];
        test_harness::check_eq!(node.write(&source).expect("populate"), size);
        node.seek(0);
        test_harness::check_eq!(node.read(&mut target).expect("warm read"), size);
        test_harness::check_eq!(target, source);
        let rounds = rounds_for(size);
        let read = nanos_per_op(rounds, || {
            node.seek(0);
            test_harness::check_eq!(node.read(&mut target).expect("read"), size);
            core::hint::black_box(&target);
        });
        let write = nanos_per_op(rounds, || {
            node.seek(0);
            test_harness::check_eq!(node.write(&source).expect("write"), size);
        });
        node.seek(0);
        node.read(&mut target).expect("verify");
        test_harness::check_eq!(target, source);
        test_harness::report!("fs api bytes={size} read_ns_per_op={read} write_ns_per_op={write}");
    }
}

#[test_harness::test(bench)]
fn bench_plain_memory() {
    for size in SIZES {
        let source = vec![0x5A_u8; size];
        let mut target = vec![0_u8; size];
        let copied = nanos_per_op(rounds_for(size), || {
            target.copy_from_slice(&source);
            core::hint::black_box(&target);
        });
        test_harness::check_eq!(source, target);
        test_harness::report!("mem copy bytes={size} ns_per_op={copied}");
    }
}

#[test_harness::test(bench)]
fn bench_mapped_memory() {
    let resource = Resource::self_resource().expect("resource");
    for size in SIZES {
        let region = resource
            .create_virtual(
                (size as u64).div_ceil(USERLAND_PAGE_SIZE) * USERLAND_PAGE_SIZE,
                MemoryAccess::RW,
            )
            .expect("region");
        let mapping = Mapping::full(region, UserMemFlags::ReadWrite).expect("mapping");
        let source = vec![0x5A; size];
        let mut target = vec![0; size];
        let copied = nanos_per_op(rounds_for(size), || {
            mapping.copy_in(&source).expect("copy in");
            mapping.copy_out(&mut target).expect("copy out");
            core::hint::black_box(&target);
        });
        test_harness::check_eq!(source, target);
        test_harness::report!("mem mapped round_trip bytes={size} ns_per_op={copied}");
    }
}
