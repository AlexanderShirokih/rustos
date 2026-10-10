//! E2E проверка границ процесса из EL0: выдача сверх бюджета метеринг-ресурса,
//! создание объекта при заполненной таблице handle и отображение при заполненной
//! таблице отображений отвергаются ошибкой, освобождение возвращает выданное, а
//! процесс после отказа продолжает работу.

use alloc::vec::Vec;

use kernel_objects::{Error, MemoryRegion, Port, Resource, Signal};
use runtime::{MemoryAccess, SUCCESS_EXIT_CODE};
use syscall::{HANDLE_TABLE_CAPACITY, Rights, SyscallError};
use userland::USERLAND_PAGE_SIZE;

use crate::fixture::{JOIN_TIMEOUT, spawn_entry};

const MAPPING_TABLE_FIXTURE: &str = "/mapping-table-full-fixture/exec";

const PROBE_BUDGET_PAGES: u64 = 64;

const PARTIAL_RELEASE_PAGES: u64 = 16;

pub(crate) struct SaturatedBudget {
    origin: Resource,
    resource: Resource,
    granted: Vec<MemoryRegion>,
    refusal: Error,
}

impl SaturatedBudget {
    pub(crate) fn probe() -> Self {
        // Производный ресурс - капа, деривированная от хэндла исходного. Закрытие
        // исходного отзывает его, поэтому проба держит оба.
        let origin = Resource::self_resource().expect("self metering resource");
        let resource = origin
            .split(PROBE_BUDGET_PAGES * USERLAND_PAGE_SIZE)
            .expect("split");
        let mut granted = Vec::new();
        let refusal = fill(&resource, &mut granted);

        Self {
            origin,
            resource,
            granted,
            refusal,
        }
    }

    pub(crate) fn granted_pages(&self) -> u64 {
        self.granted.len() as u64
    }

    pub(crate) fn refusal(&self) -> Error {
        self.refusal
    }

    pub(crate) fn saturate(&mut self) -> u64 {
        let held = self.granted.len();
        self.refusal = fill(&self.resource, &mut self.granted);
        (self.granted.len() - held) as u64
    }

    fn origin(&self) -> &Resource {
        &self.origin
    }

    fn release(&mut self, pages: u64) {
        let pages = (pages as usize).min(self.granted.len());
        self.granted.truncate(self.granted.len() - pages);
    }

    fn request(&self, pages: u64) -> kernel_objects::Result<MemoryRegion> {
        self.resource
            .create_virtual(pages * USERLAND_PAGE_SIZE, MemoryAccess::RW)
    }
}

fn fill(resource: &Resource, granted: &mut Vec<MemoryRegion>) -> Error {
    loop {
        match resource.create_virtual(USERLAND_PAGE_SIZE, MemoryAccess::RW) {
            Ok(region) => granted.push(region),
            Err(refusal) => break refusal,
        }
    }
}

#[test_harness::test]
fn budget_exhaustion_is_refused_by_its_own_code() {
    let probe = SaturatedBudget::probe();

    test_harness::check_eq!(probe.granted_pages(), PROBE_BUDGET_PAGES);
    test_harness::check!(matches!(
        probe.refusal(),
        Error::Syscall(SyscallError::ResourceExhausted)
    ));

    for _ in 0..4 {
        test_harness::check_eq!(
            probe.request(1).err(),
            Some(Error::Syscall(SyscallError::ResourceExhausted))
        );
    }

    let region = probe
        .origin()
        .create_virtual(USERLAND_PAGE_SIZE, MemoryAccess::RW)
        .expect("the origin budget outlives the refusal");
    drop(region);
}

#[test_harness::test]
fn a_released_budget_yields_the_same_pages_again() {
    let mut probe = SaturatedBudget::probe();
    let granted = probe.granted_pages();
    probe.release(granted);

    test_harness::check_eq!(probe.saturate(), granted);
}

#[test_harness::test]
fn a_partial_release_returns_exactly_what_was_freed() {
    let mut probe = SaturatedBudget::probe();
    probe.release(PARTIAL_RELEASE_PAGES);

    let regained = probe
        .request(PARTIAL_RELEASE_PAGES)
        .expect("the released pages are back in the budget");

    test_harness::check_eq!(
        probe.request(1).err(),
        Some(Error::Syscall(SyscallError::ResourceExhausted))
    );
    drop(regained);
}

pub(crate) fn exhaust_handle_table() -> (Port, Vec<Port>) {
    let source = Port::create().expect("filler port");
    let mut handles = Vec::new();
    loop {
        match source.duplicate(Rights::TRANSFER, None) {
            Ok(handle) => handles.push(handle),
            Err(error) => {
                test_harness::check!(matches!(error, Error::Syscall(SyscallError::OutOfHandles)));
                break;
            }
        }
    }
    (source, handles)
}

fn fill_handle_table() -> (Vec<Signal>, Option<Error>) {
    let mut held = Vec::with_capacity(HANDLE_TABLE_CAPACITY as usize);
    let mut refusal = None;
    for _ in 0..=HANDLE_TABLE_CAPACITY {
        match Signal::create() {
            Ok(signal) => held.push(signal),
            Err(error) => {
                refusal = Some(error);
                break;
            }
        }
    }
    (held, refusal)
}

#[test_harness::test]
fn a_full_handle_table_refuses_the_next_object() {
    let (held, refusal) = fill_handle_table();
    drop(held);

    test_harness::check_eq!(refusal, Some(Error::Syscall(SyscallError::OutOfHandles)));

    let signal = Signal::create().expect("the table takes objects after the refusal");
    drop(signal);
}

#[test_harness::test]
fn closing_a_handle_makes_room_after_the_refusal() {
    let (mut held, _) = fill_handle_table();
    held.pop().expect("the table took at least one object");

    let taken = Signal::create();
    let over = Signal::create().err();
    let slot_taken = taken.is_ok();
    drop(taken);
    drop(held);

    test_harness::check!(slot_taken);
    test_harness::check_eq!(over, Some(Error::Syscall(SyscallError::OutOfHandles)));
}

#[test_harness::test]
fn a_full_mapping_table_refuses_by_its_own_code() {
    let resource = Resource::self_resource().expect("self metering resource");
    let probe = resource
        .create_virtual(USERLAND_PAGE_SIZE, MemoryAccess::RW)
        .expect("probe region");
    let lease = probe
        .duplicate(Rights::READ | Rights::WRITE | Rights::TRANSFER, None)
        .expect("probe region for the fixture");
    let process = spawn_entry(MAPPING_TABLE_FIXTURE, lease);

    process.join(JOIN_TIMEOUT).expect("fixture terminates");
    test_harness::check_eq!(process.exit_code(), Ok(SUCCESS_EXIT_CODE.cast_unsigned()));
}
