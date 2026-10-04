//! E2E проверка границ процесса из EL0: выдача сверх бюджета метеринг-ресурса
//! и создание объекта при заполненной таблице handle отвергаются ошибкой,
//! освобождение возвращает выданное, а процесс после отказа продолжает работу.

use alloc::vec::Vec;

use kernel_objects::{Error, MemoryRegion, Resource, Signal};
use kernel_tests::kernel_test;
use runtime::MemoryAccess;
use syscall::{HANDLE_TABLE_CAPACITY, SyscallError};
use userland::USERLAND_PAGE_SIZE;

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

#[kernel_test]
fn budget_exhaustion_is_refused_by_its_own_code() {
    let probe = SaturatedBudget::probe();

    kernel_tests::kassert_eq!(probe.granted_pages(), PROBE_BUDGET_PAGES);
    kernel_tests::kassert!(matches!(
        probe.refusal(),
        Error::Syscall(SyscallError::ResourceExhausted)
    ));

    for _ in 0..4 {
        kernel_tests::kassert_eq!(
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

#[kernel_test]
fn a_released_budget_yields_the_same_pages_again() {
    let mut probe = SaturatedBudget::probe();
    let granted = probe.granted_pages();
    probe.release(granted);

    kernel_tests::kassert_eq!(probe.saturate(), granted);
}

#[kernel_test]
fn a_partial_release_returns_exactly_what_was_freed() {
    let mut probe = SaturatedBudget::probe();
    probe.release(PARTIAL_RELEASE_PAGES);

    let regained = probe
        .request(PARTIAL_RELEASE_PAGES)
        .expect("the released pages are back in the budget");

    kernel_tests::kassert_eq!(
        probe.request(1).err(),
        Some(Error::Syscall(SyscallError::ResourceExhausted))
    );
    drop(regained);
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

#[kernel_test]
fn a_full_handle_table_refuses_the_next_object() {
    let (held, refusal) = fill_handle_table();
    drop(held);

    kernel_tests::kassert_eq!(refusal, Some(Error::Syscall(SyscallError::OutOfHandles)));

    let signal = Signal::create().expect("the table takes objects after the refusal");
    drop(signal);
}

#[kernel_test]
fn closing_a_handle_makes_room_after_the_refusal() {
    let (mut held, _) = fill_handle_table();
    held.pop().expect("the table took at least one object");

    let taken = Signal::create();
    let over = Signal::create().err();
    let slot_taken = taken.is_ok();
    drop(taken);
    drop(held);

    kernel_tests::kassert!(slot_taken);
    kernel_tests::kassert_eq!(over, Some(Error::Syscall(SyscallError::OutOfHandles)));
}
