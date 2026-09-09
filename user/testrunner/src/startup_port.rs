//! Startup-порт компонента открыт всю incarnation. После стартовой транзакции он
//! устанавливает сессии экспортов, а уход держателей этих сессий компонент не завершает.

use core::num::NonZeroU64;

use component_abi::{SESSION_SCOPE_MAX, SessionError, component_startup};
use filesystem_abi::{
    AccessMode, NodeKind, OpenFlags, PATH_MAX, file_system, namespace, namespace_admin,
};
use ipc::wire::Str;
use kernel_objects::{Port, Resource};
use kernel_tests::kernel_test;
use managed_component::{
    ComponentRoots, ComponentSlots, ManagedComponent, ManagedStartupError, launch_component,
};
use runtime::{PortTransport, Timeout};
use syscall::Rights;
use userland_loader::{InitializedProcess, launch_entry_retained};

use crate::{
    managed_startup::{image_mapping, image_program, process_pages},
    watched_capability::Watched,
};

const PROVIDER_ENTRY: &str = "/boot/spare-filesystem/program";
const PROVIDER_NAME: &str = "spare-filesystem";

const ROUTER_ENTRY: &str = "/boot/namespace-router/program";
const ROUTER_NAME: &str = "namespace-router";
const ROUTER_SLACK_PAGES: u64 = 256;
const ROUTER_EXPORTS: usize = 2;
const NAMESPACE_EXPORT: usize = 0;
const NAMESPACE_ADMIN_EXPORT: usize = 1;
const ROUTER_BADGE: NonZeroU64 = NonZeroU64::new(0x54).expect("badge is non-zero");
const ROUTER_LOG_BADGE: NonZeroU64 = NonZeroU64::new(0x55).expect("badge is non-zero");

const SINGLE_EXPORT: u16 = 0;
const ABSENT_EXPORT: u16 = 1;

const GUEST_BADGE: NonZeroU64 = NonZeroU64::new(0x51).expect("badge is non-zero");
const LOG_BADGE: NonZeroU64 = NonZeroU64::new(0x52).expect("badge is non-zero");
const REPEATED_BADGE: NonZeroU64 = NonZeroU64::new(0x53).expect("badge is non-zero");

const DELIVERY_TIMEOUT: Timeout = Timeout::from_secs(1);
const CALL_TIMEOUT: Timeout = Timeout::from_secs(5);
const PROVIDER_TURN: Timeout = Timeout::from_millis(200);
const PROCESS_TIMEOUT: Timeout = Timeout::from_secs(5);

const STOP_EXIT_CODE: u32 = 9;

struct Provider {
    process: kernel_objects::Process,
    startup: Port,
    minter: Port,
}

impl Provider {
    fn raise() -> Self {
        let mapping = image_mapping();
        let program = image_program(&mapping, PROVIDER_ENTRY);
        let resource = Resource::self_resource().expect("self metering resource");

        let export = Port::create().expect("export port");
        let rights = Rights::WRITE | Rights::TRANSFER | Rights::DUPLICATE;
        // Минтер выводится из корня, пока тот у вызывающего, потому что заклеймённая капа
        // значка уже не примет.
        let minter = export
            .duplicate(rights, None)
            .expect("export minter minted");
        let log = crate::log_relay::component_log_session(LOG_BADGE).expect("log session minted");
        let export = export;

        let InitializedProcess { process, startup } = launch_entry_retained(
            &program,
            Some(PROVIDER_NAME),
            &resource,
            None,
            move |startup| -> Result<(), ManagedStartupError> {
                let transport =
                    PortTransport::client(startup.as_raw()).with_send_timeout(DELIVERY_TIMEOUT);
                let client = component_startup::Client::new(transport);
                client.begin(log, 1, 0)?;
                client.export(export).map_err(ManagedStartupError::from)
            },
        )
        .expect("startup transaction delivered");

        Self {
            process,
            startup,
            minter,
        }
    }

    fn client(&self) -> component_startup::Client<PortTransport> {
        component_startup::Client::new(PortTransport::client(self.startup.as_raw()))
            .with_wait(CALL_TIMEOUT)
    }

    fn open_session(&self, export: u16) -> Result<Port, SessionError> {
        let mut response = self
            .client()
            .open_session(export, whole_scope())
            .expect("open_session call");
        let capability = response.value?;
        let session = response.claim(capability).expect("capability owner");
        Ok(Port::from(session))
    }

    fn stop(self) {
        self.process.terminate(STOP_EXIT_CODE).expect("terminate");
        self.process.join(PROCESS_TIMEOUT).expect("provider stops");
    }
}

fn whole_scope() -> Str<'static, SESSION_SCOPE_MAX> {
    Str::new("").expect("the empty scope fits the bound")
}

/// Кадр стартовой транзакции после её конца отвергается, а порт обслуживает сессии дальше.
#[kernel_test]
fn a_repeated_begin_is_refused_and_the_startup_port_keeps_serving() {
    let provider = Provider::raise();
    let repeated = Watched::create(REPEATED_BADGE);

    // Приёмник транзакции сменился обслуживанием сессий, и capability повторного кадра
    // уходит вместе с владеющим кадром.
    provider
        .client()
        .begin(repeated.deliver(), 1, 0)
        .expect("repeated begin delivered");
    repeated.assert_closed();

    let session = provider
        .open_session(SINGLE_EXPORT)
        .expect("startup port answers after the repeated frame");
    drop(session);
    provider.stop();
}

/// Уход держателя сессии экспорта компонент не завершает.
#[kernel_test]
fn a_departed_export_consumer_leaves_the_component_running() {
    let provider = Provider::raise();

    let guest = provider
        .minter
        .duplicate(Rights::WRITE | Rights::TRANSFER, Some(GUEST_BADGE))
        .expect("export session minted");
    let session = provider
        .open_session(SINGLE_EXPORT)
        .expect("session established");
    // Ответ ставит цикл обслуживания экспорта в известное состояние до ухода значка.
    let root = Str::<PATH_MAX>::new("/").expect("the root path fits the bound");
    let opened = file_system::Client::new(PortTransport::client(&session))
        .with_wait(CALL_TIMEOUT)
        .open(root, AccessMode::Read, OpenFlags::NONE)
        .expect("export port answers its session")
        .value
        .expect("the root of the session opens");
    kernel_tests::kassert_eq!(opened.kind, NodeKind::Directory);

    drop(session);
    drop(guest);
    kernel_tests::kassert!(provider.process.join(PROVIDER_TURN).is_err());

    provider.stop();
}

/// Индекс за пределами экспортов компонента сессии не даёт.
#[kernel_test]
fn a_session_of_an_absent_export_is_refused() {
    let provider = Provider::raise();

    kernel_tests::kassert_eq!(
        provider.open_session(ABSENT_EXPORT).err(),
        Some(SessionError::Refused)
    );

    provider.stop();
}

/// Порты экспортов приходят компоненту в порядке объявления его манифеста.
#[kernel_test]
fn a_two_export_component_receives_its_ports_in_declaration_order() {
    let mapping = image_mapping();
    let program = image_program(&mapping, ROUTER_ENTRY);
    let parent = Resource::self_resource().expect("self metering resource");
    let mut component = ManagedComponent::<ComponentRoots>::new(
        &parent,
        process_pages(&program) + ROUTER_SLACK_PAGES,
    )
    .expect("reserve the component budget");
    let log = crate::log_relay::component_log_session(ROUTER_LOG_BADGE).expect("log session");

    launch_component(
        &mut component,
        &program,
        ROUTER_NAME,
        &log,
        CALL_TIMEOUT,
        ROUTER_EXPORTS,
        ComponentSlots::default(),
    )
    .expect("startup transaction delivered");
    let roots = component.running().expect("component is running").roots();

    // Маршрутизатор объявляет `Namespace` первым экспортом, а `NamespaceAdmin` вторым.
    let service = roots
        .session(NAMESPACE_EXPORT, ROUTER_BADGE)
        .expect("namespace session minted");
    let admin = roots
        .session(NAMESPACE_ADMIN_EXPORT, ROUTER_BADGE)
        .expect("admin session minted");
    let root = Str::<PATH_MAX>::new("/").expect("the root path fits the bound");

    kernel_tests::kassert!(
        namespace::Client::new(PortTransport::client(&service))
            .with_wait(CALL_TIMEOUT)
            .resolve(root)
            .is_ok()
    );
    kernel_tests::kassert!(
        namespace_admin::Client::new(PortTransport::client(&admin))
            .with_wait(CALL_TIMEOUT)
            .unmount(root)
            .is_ok()
    );
    // Порт первого экспорта административного ordinal не разбирает, и кадр отвергается целиком.
    kernel_tests::kassert_eq!(
        namespace_admin::Client::new(PortTransport::client(&service))
            .with_wait(CALL_TIMEOUT)
            .unmount(root)
            .err(),
        Some(ipc::wire::IpcError::PeerClosed)
    );

    drop(service);
    drop(admin);
    component
        .stop(STOP_EXIT_CODE, PROCESS_TIMEOUT)
        .expect("router stops");
}
