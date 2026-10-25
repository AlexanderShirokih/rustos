//! E2E сессии `SystemInfo` из EL0: сверка величин снимка с деревом устройств и
//! расходом фреймов.

use bootstrap_abi::{SystemSnapshot, system_info};
use fdt::{devicetree::DeviceTree, devicetreeext::NodeExt};
use kernel_objects::{MemoryRegion, ReadOnlyMapping, Resource, SessionTransport};
use runtime::{Timeout, UserMemFlags};

use crate::composition::pause;

const ALLOCATION_PAGES: u64 = 16;

const SETTLE_STEP: Timeout = Timeout::from_millis(10);
const SETTLE_MATCHES: usize = 8;
const SETTLE_STEPS: usize = 500;

type SystemInfo = system_info::Client<SessionTransport>;

fn system_info() -> SystemInfo {
    crate::system_info_session().client()
}

fn snapshot(system_info: &SystemInfo) -> SystemSnapshot {
    system_info.snapshot().wait().expect("snapshot answered")
}

fn settled_snapshot(system_info: &SystemInfo) -> SystemSnapshot {
    // Ядро освобождает фреймы завершённых процессов прошлых тестов не сразу,
    // поэтому замер берётся, когда число свободных фреймов перестало меняться.
    let mut last = snapshot(system_info);
    let mut matches = 0;
    for _ in 0..SETTLE_STEPS {
        pause(SETTLE_STEP);
        let current = snapshot(system_info);
        if current.free_frames == last.free_frames {
            matches += 1;
            if matches == SETTLE_MATCHES {
                return current;
            }
        } else {
            matches = 0;
        }
        last = current;
    }
    last
}

fn available_cpu_nodes() -> u32 {
    let fdt = crate::hardware_session()
        .client()
        .acquire_boot_fdt()
        .wait()
        .expect("boot fdt granted");

    let offset = usize::try_from(fdt.offset).expect("fdt offset fits usize");
    let region = MemoryRegion::from(fdt.region);
    let mapping = ReadOnlyMapping::new(region).expect("boot fdt maps read-only");
    let tree = mapping
        .as_bytes()
        .get(offset..)
        .and_then(|bytes| DeviceTree::from_bytes(bytes).ok())
        .expect("boot fdt parses");

    let cpus = tree.find("/cpus").expect("boot fdt describes /cpus");
    let available = cpus
        .children()
        .filter(|node| {
            node.prop("device_type").and_then(|kind| kind.as_cstr()) == Some("cpu")
                && node.is_available()
        })
        .count();
    u32::try_from(available).expect("cpu count fits u32")
}

#[test_harness::test]
fn snapshot_counts_every_available_cpu_of_the_boot_fdt() {
    test_harness::check_eq!(snapshot(&system_info()).cpus, available_cpu_nodes());
}

#[test_harness::test]
fn mapped_pages_take_free_frames_and_keep_managed_ones() {
    let system_info = system_info();
    let before = settled_snapshot(&system_info);

    test_harness::check!(before.free_frames <= before.managed_frames);
    test_harness::check_eq!(before.frame_size, syscall::PAGE_SIZE as u32);

    let page = u64::from(before.frame_size);
    let resource = Resource::self_resource().expect("self metering resource");
    let mapping = resource
        .allocate(ALLOCATION_PAGES * page, UserMemFlags::ReadWrite)
        .expect("allocate mapped pages");
    for index in 0..ALLOCATION_PAGES {
        let slot = usize::try_from(mapping.va() + index * page).expect("va fits usize") as *mut u8;
        // SAFETY: allocate выдал RW-окно в ALLOCATION_PAGES страниц; байт по базе
        // каждой страницы лежит в его границах, а маппингом владеет только этот тест.
        unsafe {
            slot.write_volatile(1);
        }
    }

    let after = snapshot(&system_info);
    drop(mapping);

    test_harness::check_eq!(after.managed_frames, before.managed_frames);
    test_harness::check!(after.free_frames < before.free_frames);
}
