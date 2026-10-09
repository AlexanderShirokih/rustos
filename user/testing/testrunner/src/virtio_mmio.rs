//! Шина `virtio-mmio` на транспортах QEMU `virt` с `virtio-gpu-device`.

use alloc::string::String;

use component_abi::RequirementDeclaration;
use ipc::{CallError, Protocol};
use kernel_objects::{Error, IrqLine, Session, SessionTransport};
use runtime::Timeout;
use syscall::SyscallError;
use virtio::{DeviceType, register};
use virtio_mmio_abi::{SlotError, VirtioMmioError, virtio_mmio, virtio_slot};

use crate::{
    composition::{Importer, pause, request},
    fixture::{JOIN_TIMEOUT, spawn_entry},
    log_relay::await_component_lines,
    rootkeeper_system::{
        FIRST_VIRTIO, POLL_ATTEMPTS, POLL_INTERVAL, finish, platform_describes, with_drivers,
    },
};

const BUS: &str = "virtio-mmio";

const SLOT_FIXTURE: &str = "/virtio-slot-fixture/exec";

const QEMU_VIRT_TAKEN: &str = "32 transports taken";

const BUS_REPLY_TIMEOUT: Timeout = Timeout::from_secs(5);

const GPU: u32 = DeviceType::Gpu as u32;
const NETWORK: u32 = 1;
const EMPTY: u32 = 0;

const BUS_IMPORTS: &[crate::composition::Import] = &[crate::composition::Import {
    declaration: RequirementDeclaration::all(BUS, virtio_mmio::Protocol::ID),
    namespace_root: None,
}];

fn bus_session(importer: &Importer) -> virtio_mmio::Client<SessionTransport> {
    request::<virtio_mmio::Protocol>(importer.handle(), BUS, BUS_REPLY_TIMEOUT)
        .expect("bus session")
        .client()
        .with_wait(BUS_REPLY_TIMEOUT)
}

fn open(
    bus: &virtio_mmio::Client<SessionTransport>,
    device_type: u32,
) -> Result<Session<virtio_slot::Protocol>, CallError<VirtioMmioError>> {
    bus.open(device_type).map(Session::from_session)
}

fn client(slot: Session<virtio_slot::Protocol>) -> virtio_slot::Client<SessionTransport> {
    slot.client().with_wait(BUS_REPLY_TIMEOUT)
}

fn reopen(bus: &virtio_mmio::Client<SessionTransport>) -> Option<Session<virtio_slot::Protocol>> {
    // Уход клиента шина обрабатывает после его выхода, и до сброса транспорт занят.
    (0..POLL_ATTEMPTS).find_map(|_| match open(bus, GPU) {
        Err(CallError::Remote(VirtioMmioError::Busy)) => {
            pause(POLL_INTERVAL);
            None
        }
        opened => Some(opened.expect("gpu slot after the reset")),
    })
}

fn offset(register: usize) -> u16 {
    u16::try_from(register).expect("register offset fits u16")
}

#[test_harness::test]
fn a_gpu_slot_reads_the_gpu_device_id_and_a_second_open_is_busy() {
    if !platform_describes(FIRST_VIRTIO) {
        test_harness::skip!("{} is not described", FIRST_VIRTIO);
    }

    let composition = with_drivers();
    let importer = composition.importer(BUS_IMPORTS);
    let bus = bus_session(&importer);

    let gpu = client(open(&bus, GPU).expect("gpu slot"));
    let device_id = gpu.read(offset(register::DEVICE_ID));
    let second = bus_session(&importer);
    let busy = open(&second, GPU).map(drop);
    // Транспорт, окно которого не выдано, шина пропускает строкой журнала до итоговой.
    let lines = await_component_lines(BUS, 1);

    test_harness::check_eq!(device_id, Ok(GPU));
    test_harness::check_eq!(busy, Err(CallError::Remote(VirtioMmioError::Busy)));
    test_harness::check_eq!(lines.first().map(String::as_str), Some(QEMU_VIRT_TAKEN));
    drop((gpu, bus, second));
    finish(composition);
}

#[test_harness::test]
fn an_absent_or_empty_device_type_is_no_device() {
    if !platform_describes(FIRST_VIRTIO) {
        test_harness::skip!("{} is not described", FIRST_VIRTIO);
    }

    let composition = with_drivers();
    let importer = composition.importer(BUS_IMPORTS);
    let bus = bus_session(&importer);

    let absent = open(&bus, NETWORK).map(drop);
    let empty = open(&bus, EMPTY).map(drop);

    test_harness::check_eq!(absent, Err(CallError::Remote(VirtioMmioError::NoDevice)));
    test_harness::check_eq!(empty, Err(CallError::Remote(VirtioMmioError::NoDevice)));
    drop(bus);
    finish(composition);
}

#[test_harness::test]
fn a_register_past_the_window_or_unaligned_is_out_of_range() {
    if !platform_describes(FIRST_VIRTIO) {
        test_harness::skip!("{} is not described", FIRST_VIRTIO);
    }

    let composition = with_drivers();
    let importer = composition.importer(BUS_IMPORTS);
    let bus = bus_session(&importer);
    let gpu = client(open(&bus, GPU).expect("gpu slot"));
    let status = offset(register::STATUS);

    let past_window = gpu.write(u16::MAX - 3, 1);
    let unaligned = gpu.write(status + 1, 1);
    let read_past = gpu.read(u16::MAX - 3);
    let untouched = gpu.read(status);

    test_harness::check_eq!(past_window, Err(CallError::Remote(SlotError::OutOfRange)));
    test_harness::check_eq!(unaligned, Err(CallError::Remote(SlotError::OutOfRange)));
    test_harness::check_eq!(read_past, Err(CallError::Remote(SlotError::OutOfRange)));
    test_harness::check_eq!(untouched, Ok(0));
    drop((gpu, bus));
    finish(composition);
}

#[test_harness::test]
fn the_slot_of_a_terminated_client_reopens_with_the_device_reset() {
    if !platform_describes(FIRST_VIRTIO) {
        test_harness::skip!("{} is not described", FIRST_VIRTIO);
    }

    let composition = with_drivers();
    let importer = composition.importer(BUS_IMPORTS);
    let bus = bus_session(&importer);
    let slot = open(&bus, GPU).expect("gpu slot");

    let fixture = spawn_entry(SLOT_FIXTURE, slot.into_handle());
    fixture.join(JOIN_TIMEOUT).expect("slot client terminates");
    let status = reopen(&bus)
        .map(client)
        .map(|gpu| gpu.read(offset(register::STATUS)));

    test_harness::check_eq!(fixture.exit_code().expect("exit code"), 0);
    test_harness::check_eq!(status, Some(Ok(0)));
    drop(bus);
    finish(composition);
}

#[test_harness::test]
fn the_line_of_a_closed_slot_is_revoked() {
    if !platform_describes(FIRST_VIRTIO) {
        test_harness::skip!("{} is not described", FIRST_VIRTIO);
    }

    let composition = with_drivers();
    let importer = composition.importer(BUS_IMPORTS);
    let bus = bus_session(&importer);
    let gpu = client(open(&bus, GPU).expect("gpu slot"));
    let line = IrqLine::from(gpu.irq().expect("gpu line"));

    let held = line.ack().err();
    drop(gpu);
    let reopened = reopen(&bus);
    let released = line.ack().err();

    test_harness::check!(held != Some(Error::Syscall(SyscallError::Revoked)));
    test_harness::check!(reopened.is_some());
    test_harness::check_eq!(released, Some(Error::Syscall(SyscallError::Revoked)));
    drop((reopened, bus));
    finish(composition);
}
