//! Клиентская сторона `port-fixture`: спавн потомка из userland-образа и
//! запрос-ответ по его bootstrap-порту.

use core::num::NonZeroU64;

use kernel_objects::{ObjectKind, OwnedHandle, Port, Process, Resource, Result};
use runtime::Timeout;
use syscall::ProcessName;
use userland::{DecodedProgram, ProgramView, SegmentPermissions, SegmentView, USERLAND_PAGE_SIZE};
use userland_image::component_name;
use userland_loader::{ImagePages, load_program};

use crate::{composition::image, fixture_protocol::encode_request, ipc_raw};

const REQUEST_TIMEOUT: Timeout = Timeout::from_secs(5);
pub(crate) const JOIN_TIMEOUT: Timeout = Timeout::from_secs(5);

pub(crate) const FIXTURE_ENTRY: &str = "/port-fixture/exec";

const FIXTURE_SESSION_BADGE: NonZeroU64 = NonZeroU64::new(1).expect("badge is non-zero");

pub(crate) enum Metering<'a> {
    Inherited,
    Split(u64),
    Provided(&'a Resource),
}

pub(crate) struct Fixture {
    process: Process,
    port: Port,
    budget_bytes: Option<u64>,
}

impl Fixture {
    pub(crate) fn spawn() -> Self {
        Self::spawn_metered_by(&Metering::Inherited, Some(&image().pages()))
            .expect("spawn port fixture")
    }

    pub(crate) fn spawn_with_slack(slack_bytes: u64) -> Self {
        Self::try_spawn_with_slack(slack_bytes).expect("spawn port fixture")
    }

    pub(crate) fn try_spawn_with_slack(slack_bytes: u64) -> Result<Self> {
        let budget_bytes = writable_bytes(&program(FIXTURE_ENTRY)) + slack_bytes;
        Self::spawn_split(budget_bytes, Some(&image().pages()))
    }

    pub(crate) fn spawn_split(budget_bytes: u64, pages: Option<&ImagePages<'_>>) -> Result<Self> {
        Self::spawn_metered_by(&Metering::Split(budget_bytes), pages)
    }

    pub(crate) fn spawn_on(resource: &Resource) -> Self {
        Self::spawn_metered_by(&Metering::Provided(resource), Some(&image().pages()))
            .expect("spawn port fixture")
    }

    pub(crate) fn budget_bytes(&self) -> u64 {
        self.budget_bytes
            .expect("fixture spawned on a split budget")
    }

    fn spawn_metered_by(metering: &Metering<'_>, pages: Option<&ImagePages<'_>>) -> Result<Self> {
        let program = program(FIXTURE_ENTRY);

        let mut budget_bytes = None;
        let own_resource;
        // Капа-источник производного ресурса остаётся действительной до конца загрузки. Её
        // закрытие отзывает производную капу раньше, чем та оплатит сегменты.
        let split_parent;
        let resource = match metering {
            Metering::Provided(resource) => *resource,
            Metering::Inherited => {
                own_resource = Resource::self_resource().expect("self metering resource");
                &own_resource
            }
            Metering::Split(bytes) => {
                budget_bytes = Some(*bytes);
                split_parent = Resource::self_resource().expect("self metering resource");
                own_resource = split_parent.split(*bytes).expect("split the child budget");
                &own_resource
            }
        };
        let service = Port::create_service(Some(FIXTURE_SESSION_BADGE))?;
        let process = load_program(
            &program,
            pages,
            ProcessName::new(component_name(FIXTURE_ENTRY)).expect("fixture process name"),
            resource,
            service.server,
        )?;

        Ok(Self {
            process,
            port: service.client,
            budget_bytes,
        })
    }

    pub(crate) fn request(&self, op: u8, arg: u64, caps: &[u32], out: &mut [u8]) -> usize {
        ipc_raw::write_frame(&encode_request(op, arg), caps);
        self.port
            .call(REQUEST_TIMEOUT)
            .expect("fixture answers the request");
        let (len, _) = ipc_raw::read_frame(out);
        len
    }

    pub(crate) fn request_status(&self, op: u8, arg: u64, caps: &[u32]) -> u8 {
        let mut out = [0u8; 1];
        let len = self.request(op, arg, caps, &mut out);
        test_harness::check_eq!(len, 1);
        out[0]
    }

    pub(crate) fn process(&self) -> &Process {
        &self.process
    }

    pub(crate) fn into_process(self) -> Process {
        self.into_parts().0
    }

    pub(crate) fn into_parts(self) -> (Process, Port) {
        (self.process, self.port)
    }
}

fn writable_bytes<'a, E: ProgramView<'a>>(program: &E) -> u64 {
    page_bytes(
        program
            .segments()
            .filter(|segment| segment.permissions() == SegmentPermissions::ReadWrite),
    )
}

pub(crate) fn shared_bytes<'a, E: ProgramView<'a>>(program: &E) -> u64 {
    page_bytes(
        program
            .segments()
            .filter(|segment| segment.permissions() != SegmentPermissions::ReadWrite),
    )
}

fn page_bytes<'a>(segments: impl Iterator<Item = impl SegmentView<'a>>) -> u64 {
    segments
        .map(|segment| segment.mem_size().next_multiple_of(USERLAND_PAGE_SIZE))
        .sum()
}

pub(crate) fn program(path: &str) -> DecodedProgram<'static> {
    let entry = image()
        .entries()
        .expect("userland image decodes")
        .entry(path)
        .expect("image carries the entry");
    userland::decode_program(entry.data()).expect("image entry is a program")
}

pub(crate) fn spawn_entry<K: ObjectKind>(path: &str, start: OwnedHandle<K>) -> Process {
    let resource = Resource::self_resource().expect("self metering resource");
    load_program(
        &program(path),
        Some(&image().pages()),
        ProcessName::new(component_name(path)).expect("fixture process name"),
        &resource,
        start,
    )
    .expect("spawn from image")
}

pub(crate) fn process_bytes<'a, E: ProgramView<'a>>(program: &E) -> u64 {
    writable_bytes(program) + program.stack_size().next_multiple_of(USERLAND_PAGE_SIZE)
}
