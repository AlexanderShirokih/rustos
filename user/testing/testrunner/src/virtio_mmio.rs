//! Шина `virtio-mmio` на транспортах QEMU `virt` с `virtio-gpu-device`.

use alloc::string::String;

use component_abi::RequirementDeclaration;
use executor::PortChannel;
use ipc::{CallError, Protocol};
use kernel_objects::{Error, IrqLine, Session, SessionTransport};
use runtime::Timeout;
use syscall::SyscallError;
use virtio::{DeviceType, register};
use virtio_driver::open_slot;
use virtio_mmio_abi::{AccessWidth, SlotError, VirtioMmioError, virtio_mmio, virtio_slot};

use crate::{
    composition::{Import, Importer, request},
    fixture::{JOIN_TIMEOUT, spawn_entry},
    log_relay::await_component_lines,
    rootkeeper_system::{FIRST_VIRTIO, finish, platform_describes, with_drivers},
};

const BUS: &str = "virtio-mmio";

const SLOT_FIXTURE: &str = "/virtio-slot-fixture/exec";

const QEMU_VIRT_TAKEN: &str = "32 transports taken";

const BUS_REPLY_TIMEOUT: Timeout = Timeout::from_secs(5);

const NUM_SCANOUTS: u16 = 8;
const QEMU_GPU_SCANOUTS: u32 = 1;

const GPU: u32 = DeviceType::Gpu as u32;
const NETWORK: u32 = 1;
const EMPTY: u32 = 0;

pub(crate) const BUS_IMPORT: Import = Import {
    declaration: RequirementDeclaration::all(BUS, virtio_mmio::Protocol::ID),
    namespace_root: None,
};

pub(crate) fn bus_session(importer: &Importer) -> virtio_mmio::Client<SessionTransport> {
    request::<virtio_mmio::Protocol>(importer.handle(), BUS, BUS_REPLY_TIMEOUT)
        .expect("bus session")
        .client()
        .with_wait(BUS_REPLY_TIMEOUT)
}

pub(crate) fn open_transport(
    importer: &Importer,
    device: DeviceType,
) -> Result<Session<virtio_slot::Protocol>, CallError<VirtioMmioError>> {
    let executor = crate::executor::executor();
    let session = request::<virtio_mmio::Protocol>(importer.handle(), BUS, BUS_REPLY_TIMEOUT)
        .expect("bus session");
    let bus = PortChannel::client(session).with_wait(BUS_REPLY_TIMEOUT);
    executor.block_on(open_slot(&bus, device))
}

fn open(
    bus: &virtio_mmio::Client<SessionTransport>,
    device_type: u32,
) -> Result<Session<virtio_slot::Protocol>, CallError<VirtioMmioError>> {
    bus.open(device_type).wait().map(Session::from_session)
}

fn client(slot: Session<virtio_slot::Protocol>) -> virtio_slot::Client<SessionTransport> {
    slot.client().with_wait(BUS_REPLY_TIMEOUT)
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
    let importer = composition.importer(&[BUS_IMPORT]);
    let bus = bus_session(&importer);

    let gpu = client(open(&bus, GPU).expect("gpu slot"));
    let device_id = gpu.read(offset(register::DEVICE_ID)).wait();
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
    let importer = composition.importer(&[BUS_IMPORT]);
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
    let importer = composition.importer(&[BUS_IMPORT]);
    let bus = bus_session(&importer);
    let gpu = client(open(&bus, GPU).expect("gpu slot"));
    let status = offset(register::STATUS);

    let past_window = gpu.write(u16::MAX - 3, 1).wait();
    let unaligned = gpu.write(status + 1, 1).wait();
    let read_past = gpu.read(u16::MAX - 3).wait();
    let config_past = gpu.config_read(0x100, AccessWidth::Word).wait();
    let untouched = gpu.read(status).wait();

    test_harness::check_eq!(past_window, Err(CallError::Remote(SlotError::OutOfRange)));
    test_harness::check_eq!(unaligned, Err(CallError::Remote(SlotError::OutOfRange)));
    test_harness::check_eq!(read_past, Err(CallError::Remote(SlotError::OutOfRange)));
    test_harness::check_eq!(config_past, Err(CallError::Remote(SlotError::OutOfRange)));
    test_harness::check_eq!(untouched, Ok(0));
    drop((gpu, bus));
    finish(composition);
}

// Отказ невыровненного обращения не должен оставлять слот без ответа.
#[test_harness::test]
fn a_gpu_config_field_reads_with_any_width_and_an_unaligned_one_is_out_of_range() {
    if !platform_describes(FIRST_VIRTIO) {
        test_harness::skip!("{} is not described", FIRST_VIRTIO);
    }

    let composition = with_drivers();
    let importer = composition.importer(&[BUS_IMPORT]);
    let bus = bus_session(&importer);
    let gpu = client(open(&bus, GPU).expect("gpu slot"));

    let word = gpu.config_read(NUM_SCANOUTS, AccessWidth::Word).wait();
    let byte = gpu.config_read(NUM_SCANOUTS, AccessWidth::Byte).wait();
    let unaligned = gpu.config_read(2, AccessWidth::Word).wait();
    let device_id = gpu.read(offset(register::DEVICE_ID)).wait();

    test_harness::check_eq!(word, Ok(QEMU_GPU_SCANOUTS));
    test_harness::check_eq!(byte, Ok(QEMU_GPU_SCANOUTS));
    test_harness::check_eq!(unaligned, Err(CallError::Remote(SlotError::OutOfRange)));
    test_harness::check_eq!(device_id, Ok(GPU));
    drop((gpu, bus));
    finish(composition);
}

#[test_harness::test]
fn the_slot_of_a_terminated_client_reopens_with_the_device_reset() {
    if !platform_describes(FIRST_VIRTIO) {
        test_harness::skip!("{} is not described", FIRST_VIRTIO);
    }

    let composition = with_drivers();
    let importer = composition.importer(&[BUS_IMPORT]);
    let bus = bus_session(&importer);
    let slot = open(&bus, GPU).expect("gpu slot");

    let fixture = spawn_entry(SLOT_FIXTURE, slot.into_handle());
    fixture.join(JOIN_TIMEOUT).expect("slot client terminates");
    let status = open_transport(&importer, DeviceType::Gpu)
        .map(client)
        .map(|gpu| gpu.read(offset(register::STATUS)).wait());

    test_harness::check_eq!(fixture.exit_code().expect("exit code"), 0);
    test_harness::check_eq!(status, Ok(Ok(0)));
    drop(bus);
    finish(composition);
}

#[test_harness::test]
fn the_line_of_a_closed_slot_is_revoked() {
    if !platform_describes(FIRST_VIRTIO) {
        test_harness::skip!("{} is not described", FIRST_VIRTIO);
    }

    let composition = with_drivers();
    let importer = composition.importer(&[BUS_IMPORT]);
    let bus = bus_session(&importer);
    let gpu = client(open(&bus, GPU).expect("gpu slot"));
    let line = IrqLine::from(gpu.irq().wait().expect("gpu line"));

    let held = line.ack().err();
    drop(gpu);
    let reopened = open_transport(&importer, DeviceType::Gpu);
    let released = line.ack().err();

    test_harness::check!(held != Some(Error::Syscall(SyscallError::Revoked)));
    test_harness::check!(reopened.is_ok());
    test_harness::check_eq!(released, Some(Error::Syscall(SyscallError::Revoked)));
    drop((reopened, bus));
    finish(composition);
}
