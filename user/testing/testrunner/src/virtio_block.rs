//! Драйвер `driver-virtio-blk`: геометрия и запросы диска на синтетическом устройстве и диск
//! QEMU `virt` за шиной `virtio-mmio`.

use alloc::{collections::VecDeque, rc::Rc, vec, vec::Vec};
use core::cell::{Cell, RefCell};

use block_abi::{BlockError, BlockInfo, SESSION_WAIT, block_device};
use component_abi::{Attribute, RequirementDeclaration};
use component_startup::{Attributes, SessionError};
use driver_virtio_blk::{Channel, Disk, GeometryError, SessionBuffer, geometry};
use ipc::{CallError, Protocol};
use kernel_objects::{Mapping, MemoryRegion, SessionTransport};
use runtime::{Instant, Timeout, UserMemFlags};
use virtio::{
    DeviceType,
    block::{Config, RequestKind, SECTOR_BYTES, feature},
};
use virtio_driver::open_slot;

use crate::{
    composition::{Composition, Import, Importer},
    rootkeeper_system::{
        FIRST_VIRTIO, await_revoked, finish, pair, platform_describes, query, with_drivers,
    },
    virtio_mmio::{BUS_IMPORT, bus_session},
};

const DISK: &str = "disk";

const DISK_REPLY_TIMEOUT: Timeout = Timeout::from_secs(5);

const LARGE_BLOCK: u32 = 4096;
const SMALL_BUFFER_BYTES: u32 = 4096;

const QEMU_BLOCKS: u64 = 32768;
const TRANSFER_LIMIT_BYTES: u32 = 64 * 1024;

const IO_ERROR: u8 = 1;
const OK: u8 = 0;

const DISK_QUERY: &[Attribute<'static>] = &[pair("layout", "disk"), pair("disk", "virtio")];

const DISK_IMPORTS: &[Import] = &[Import {
    declaration: RequirementDeclaration::session(DISK, block_device::Protocol::ID),
    namespace_root: None,
}];

struct Script {
    statuses: VecDeque<Option<u8>>,
    reset: bool,
    sectors: Vec<u64>,
    resets: usize,
}

struct Scripted(Rc<RefCell<Script>>);

struct Tracked(Rc<Cell<usize>>);

impl Channel for Scripted {
    type Region = Tracked;

    fn transfer(
        &mut self,
        _kind: RequestKind,
        sector: u64,
        _region: &Tracked,
        _bytes: u32,
    ) -> Option<u8> {
        let mut script = self.0.borrow_mut();
        script.sectors.push(sector);
        script.statuses.pop_front().flatten()
    }

    fn reset(&mut self) -> bool {
        let mut script = self.0.borrow_mut();
        script.resets += 1;
        script.reset
    }
}

impl Drop for Tracked {
    fn drop(&mut self) {
        self.0.set(self.0.get() + 1);
    }
}

fn config(capacity: u64, blk_size: u32, size_max: u32) -> Config {
    Config {
        capacity,
        size_max,
        blk_size,
    }
}

fn disk(
    block_size: u32,
    statuses: &[Option<u8>],
    reset: bool,
) -> (Disk<Scripted>, Rc<RefCell<Script>>) {
    let info = geometry(feature::BLK_SIZE, config(QEMU_BLOCKS, block_size, 0))
        .expect("geometry is usable");
    let script = Rc::new(RefCell::new(Script {
        statuses: statuses.iter().copied().collect(),
        reset,
        sectors: Vec::new(),
        resets: 0,
    }));
    (Disk::new(Scripted(Rc::clone(&script)), info), script)
}

fn buffer(size: u32, released: &Rc<Cell<usize>>) -> SessionBuffer<Tracked> {
    SessionBuffer {
        region: Tracked(Rc::clone(released)),
        size,
    }
}

fn on_disk(test: impl FnOnce(&Composition, &Importer)) {
    if !platform_describes(FIRST_VIRTIO) {
        test_harness::skip!("{} is not described", FIRST_VIRTIO);
    }

    let composition = with_drivers();
    let importer = composition.importer(DISK_IMPORTS);
    test(&composition, &importer);
    drop(importer);
    finish(composition);
}

fn opened(
    importer: &Importer,
) -> Result<block_device::Client<SessionTransport>, CallError<SessionError>> {
    let bytes = query(DISK_QUERY);
    component_startup::session::<block_device::Protocol>(
        importer.handle(),
        DISK,
        Attributes::decode(&bytes).expect("query decodes"),
        DISK_REPLY_TIMEOUT,
    )
    .map(|session| session.client().with_wait(DISK_REPLY_TIMEOUT))
}

fn acquired(
    session: &block_device::Client<SessionTransport>,
    size: u32,
) -> Result<MemoryRegion, CallError<BlockError>> {
    session.acquire_buffer(size).map(MemoryRegion::from)
}

fn pattern(lba: u64, len: usize) -> Vec<u8> {
    (0..len)
        .map(|offset| (lba as usize).wrapping_add(offset) as u8 ^ 0xA5)
        .collect()
}

#[test_harness::test]
fn a_device_without_blk_size_has_blocks_of_one_sector() {
    test_harness::check_eq!(
        geometry(0, config(QEMU_BLOCKS, LARGE_BLOCK, 0)),
        Ok(BlockInfo {
            block_size: SECTOR_BYTES,
            blocks: QEMU_BLOCKS,
            transfer_max: TRANSFER_LIMIT_BYTES,
        })
    );
}

#[test_harness::test]
fn a_block_of_4096_bytes_drops_the_sectors_past_the_last_whole_block() {
    let info = geometry(feature::BLK_SIZE, config(32769, LARGE_BLOCK, 0))
        .map(|info| (info.block_size, info.blocks));

    test_harness::check_eq!(info, Ok((LARGE_BLOCK, 4096)));
}

#[test_harness::test]
fn an_unusable_geometry_is_refused() {
    let both = feature::BLK_SIZE | feature::SIZE_MAX;

    test_harness::check_eq!(
        geometry(both, config(QEMU_BLOCKS, 1000, 0)),
        Err(GeometryError::BlockSize(1000))
    );
    test_harness::check_eq!(
        geometry(both, config(QEMU_BLOCKS, 256, 0)),
        Err(GeometryError::BlockSize(256))
    );
    test_harness::check_eq!(
        geometry(both, config(7, LARGE_BLOCK, LARGE_BLOCK)),
        Err(GeometryError::NoBlocks)
    );
    test_harness::check_eq!(
        geometry(both, config(QEMU_BLOCKS, LARGE_BLOCK, LARGE_BLOCK - 1)),
        Err(GeometryError::TransferMax(LARGE_BLOCK - 1))
    );
}

#[test_harness::test]
fn a_block_of_4096_bytes_addresses_eight_sectors_per_block() {
    let (mut disk, script) = disk(LARGE_BLOCK, &[Some(OK)], true);

    let read = disk.serve(
        RequestKind::Read,
        3,
        1,
        Some(&buffer(LARGE_BLOCK, &Rc::default())),
    );

    test_harness::check_eq!(read, Ok(()));
    test_harness::check_eq!(script.borrow().sectors, vec![24]);
}

#[test_harness::test]
fn a_request_outside_the_disk_or_the_buffer_does_not_reach_the_device() {
    let (mut disk, script) = disk(SECTOR_BYTES, &[], true);
    let small = Some(&buffer(SMALL_BUFFER_BYTES, &Rc::default()));

    let overflow = disk.serve(RequestKind::Read, u64::MAX, 2, small);
    let past_end = disk.serve(RequestKind::Read, QEMU_BLOCKS, 1, small);
    let too_long = disk.serve(RequestKind::Read, 0, 9, small);
    let no_buffer = disk.serve(RequestKind::Write, 0, 1, None);
    let empty = disk.serve(RequestKind::Write, 0, 0, small);

    test_harness::check_eq!(overflow, Err(BlockError::OutOfRange));
    test_harness::check_eq!(past_end, Err(BlockError::OutOfRange));
    test_harness::check_eq!(too_long, Err(BlockError::BufferTooSmall));
    test_harness::check_eq!(no_buffer, Err(BlockError::NoBuffer));
    test_harness::check_eq!(empty, Ok(()));
    test_harness::check!(script.borrow().sectors.is_empty());
}

#[test_harness::test]
fn an_io_error_fails_only_its_own_request() {
    let (mut disk, script) = disk(SECTOR_BYTES, &[Some(IO_ERROR), Some(OK)], true);
    let small = Some(&buffer(SMALL_BUFFER_BYTES, &Rc::default()));

    let failed = disk.serve(RequestKind::Read, 0, 1, small);
    let next = disk.serve(RequestKind::Read, 0, 1, small);

    test_harness::check_eq!(failed, Err(BlockError::Device));
    test_harness::check_eq!(next, Ok(()));
    test_harness::check_eq!(script.borrow().resets, 0);
}

#[test_harness::test]
fn an_unanswered_request_fails_the_disk_and_frees_its_region_only_after_a_reset() {
    for reset in [true, false] {
        let released = Rc::new(Cell::new(0));
        let small = buffer(SECTOR_BYTES, &released);
        let (mut disk, script) = disk(SECTOR_BYTES, &[None], reset);

        let unanswered = disk.serve(RequestKind::Write, 0, 1, Some(&small));
        let next = disk.serve(RequestKind::Read, 0, 1, Some(&small));
        disk.release(small.region);

        test_harness::check_eq!(unanswered, Err(BlockError::Device));
        test_harness::check_eq!(next, Err(BlockError::Device));
        test_harness::check_eq!(script.borrow().sectors.len(), 1);
        test_harness::check_eq!(script.borrow().resets, 1);
        test_harness::check_eq!(released.get(), usize::from(reset));
    }
}

#[test_harness::test]
fn the_16_mib_qemu_disk_keeps_a_pattern_in_its_first_and_last_block() {
    on_disk(|_, importer| {
        let session = opened(importer).expect("disk session");
        let info = session.info().expect("info call");
        let region = acquired(&session, info.block_size).expect("buffer granted");
        let buffer = Mapping::full(region, UserMemFlags::ReadWrite).expect("buffer maps");
        let len = info.block_size as usize;

        test_harness::check_eq!((info.block_size, info.blocks), (SECTOR_BYTES, QEMU_BLOCKS));

        for lba in [0, info.blocks - 1] {
            let written = pattern(lba, len);
            let mut read = vec![0; len];

            buffer.copy_in(&written).expect("pattern fits the buffer");
            let write = session.write(lba, 1);
            buffer.zero_at(0, len).expect("buffer clears");
            let read_back = session.read(lba, 1);
            buffer.copy_out(&mut read).expect("block fits the buffer");

            test_harness::check_eq!(write, Ok(()));
            test_harness::check_eq!(read_back, Ok(()));
            test_harness::check!(read == written);
        }
    });
}

#[test_harness::test]
fn a_session_gets_one_buffer_within_the_transfer_limit() {
    on_disk(|_, importer| {
        let session = opened(importer).expect("disk session");
        let transfer_max = session.info().expect("info call").transfer_max;

        let oversized = acquired(&session, transfer_max + 1).map(drop);
        let region = acquired(&session, transfer_max).expect("buffer granted");
        let again = acquired(&session, SMALL_BUFFER_BYTES).map(drop);

        test_harness::check_eq!(oversized, Err(CallError::Remote(BlockError::NoSpace)));
        test_harness::check_eq!(again, Err(CallError::Remote(BlockError::AlreadyExists)));
        test_harness::check!(region.inspect().is_ok());
    });
}

#[test_harness::test]
fn a_second_session_is_refused_after_the_wait() {
    on_disk(|_, importer| {
        let _first = opened(importer).expect("disk session");

        let before = Instant::now();
        let second = opened(importer).map(drop);
        let waited = Instant::now().since(before);

        test_harness::check_eq!(second, Err(CallError::Remote(SessionError::Refused)));
        test_harness::check!(waited.raw() >= SESSION_WAIT.raw());
    });
}

#[test_harness::test]
fn a_closed_session_loses_its_buffer_and_frees_the_disk() {
    on_disk(|_, importer| {
        let first = opened(importer).expect("disk session");
        let region = acquired(&first, SMALL_BUFFER_BYTES).expect("buffer granted");

        drop(first);
        let revoked = await_revoked(|| region.inspect());
        let reopened = opened(importer).map(drop);

        test_harness::check!(revoked);
        test_harness::check_eq!(reopened, Ok(()));
    });
}

#[test_harness::test]
fn a_disk_whose_transport_is_held_is_refused() {
    on_disk(|composition, importer| {
        let bus_importer = composition.importer(&[BUS_IMPORT]);
        let bus = bus_session(&bus_importer);
        let _held = open_slot(&bus, DeviceType::Block).expect("block transport");

        let refused = opened(importer).map(drop);

        test_harness::check_eq!(refused, Err(CallError::Remote(SessionError::Refused)));
    });
}
