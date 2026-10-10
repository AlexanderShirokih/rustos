//! Клиентская сторона `port-fixture`: спавн потомка из userland-образа и
//! запрос-ответ по его bootstrap-порту.

use core::num::NonZeroU64;

use bootstrap_abi::bootstrap;
use kernel_objects::{MemoryRegion, ObjectKind, OwnedHandle, Port, Process, Resource, Result};
use runtime::{PortTransport, Timeout};
use syscall::ProcessName;
use userland::{DecodedProgram, ProgramView, SegmentView, USERLAND_PAGE_SIZE};
use userland_image::{component_name, decode};
use userland_loader::load_program;

use crate::{composition::image, fixture_protocol::encode_request, ipc_raw};

const REQUEST_TIMEOUT: Timeout = Timeout::from_secs(5);
pub(crate) const JOIN_TIMEOUT: Timeout = Timeout::from_secs(5);

pub(crate) const FIXTURE_ENTRY: &str = "/port-fixture/exec";

const FIXTURE_SESSION_BADGE: NonZeroU64 = NonZeroU64::new(1).expect("badge is non-zero");

pub(crate) enum Metering<'a> {
    Inherited,
    SplitWithSlack(u64),
    Provided(&'a Resource),
}

pub(crate) struct Fixture {
    process: Process,
    port: Port,
    budget_bytes: Option<u64>,
}

impl Fixture {
    pub(crate) fn spawn() -> Self {
        Self::spawn_metered_by(&Metering::Inherited).expect("spawn port fixture")
    }

    pub(crate) fn spawn_with_slack(slack_bytes: u64) -> Self {
        Self::try_spawn_with_slack(slack_bytes).expect("spawn port fixture")
    }

    pub(crate) fn try_spawn_with_slack(slack_bytes: u64) -> Result<Self> {
        Self::spawn_metered_by(&Metering::SplitWithSlack(slack_bytes))
    }

    pub(crate) fn spawn_on(resource: &Resource) -> Self {
        Self::spawn_metered_by(&Metering::Provided(resource)).expect("spawn port fixture")
    }

    pub(crate) fn budget_bytes(&self) -> u64 {
        self.budget_bytes
            .expect("fixture spawned on a split budget")
    }

    fn spawn_metered_by(metering: &Metering<'_>) -> Result<Self> {
        let client = bootstrap::Client::new(PortTransport::new(crate::bootstrap_handle()));
        let cap = client.acquire_userland_image().expect("vend ok");

        let region = MemoryRegion::from(cap);
        let mapping = region.map_full_read_only().expect("map image read-only");
        let image = decode(mapping.as_bytes()).expect("decode acquired image");
        let entry = image
            .entry(FIXTURE_ENTRY)
            .expect("image carries the port fixture");
        let program =
            userland::decode_program(entry.data()).expect("fixture record carries a program");

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
            Metering::SplitWithSlack(slack_bytes) => {
                let bytes = image_bytes(&program) + slack_bytes;
                budget_bytes = Some(bytes);
                split_parent = Resource::self_resource().expect("self metering resource");
                own_resource = split_parent.split(bytes).expect("split the child budget");
                &own_resource
            }
        };
        let service = Port::create_service(Some(FIXTURE_SESSION_BADGE))?;
        let process = load_program(
            &program,
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

fn image_bytes<'a, E: ProgramView<'a>>(program: &E) -> u64 {
    program
        .segments()
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
        ProcessName::new(component_name(path)).expect("fixture process name"),
        &resource,
        start,
    )
    .expect("spawn from image")
}

pub(crate) fn process_bytes<'a, E: ProgramView<'a>>(program: &E) -> u64 {
    image_bytes(program) + program.stack_size().next_multiple_of(USERLAND_PAGE_SIZE)
}
