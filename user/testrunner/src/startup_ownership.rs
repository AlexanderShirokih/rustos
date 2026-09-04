//! Владение capability стартовой транзакции. Приём, не сложившийся в контракт, закрывает всё,
//! что транзакция уже доставила.
//!
//! Каждая доставляемая capability выведена из порта вызывающего под своим значком, и уход
//! последнего держателя значка виден отказом приёма на этом порту.

use core::num::NonZeroU64;

use component_abi::component_startup;
use ipc::wire::IpcError;
use kernel_objects::{Error, OwnedHandle, Port, Process, Resource};
use kernel_tests::kernel_test;
use managed_component::ManagedStartupError;
use runtime::{PortTransport, Timeout};
use syscall::{Rights, SyscallError};
use userland_loader::{InitializedProcess, launch_entry_retained};

use crate::managed_startup::{image_mapping, image_program};

const FIXTURE_ENTRY: &str = "/boot/startup-ownership-fixture/program";
const FIXTURE_NAME: &str = "startup-ownership";

const DELIVERY_TIMEOUT: Timeout = Timeout::from_secs(1);
const CLOSURE_TIMEOUT: Timeout = Timeout::from_secs(5);

const LOG_BADGE: NonZeroU64 = NonZeroU64::new(0x71).expect("badge is non-zero");
const CONTROL_BADGE: NonZeroU64 = NonZeroU64::new(0x72).expect("badge is non-zero");
const SERVICE_BADGE: NonZeroU64 = NonZeroU64::new(0x73).expect("badge is non-zero");
const FIRST_SLOT_BADGE: NonZeroU64 = NonZeroU64::new(0x74).expect("badge is non-zero");
const SECOND_SLOT_BADGE: NonZeroU64 = NonZeroU64::new(0x75).expect("badge is non-zero");
const STARTUP_LEASE_BADGE: NonZeroU64 = NonZeroU64::new(0x76).expect("badge is non-zero");

struct Watched {
    port: Port,
    badge: NonZeroU64,
}

impl Watched {
    fn create(badge: NonZeroU64) -> Self {
        Self {
            port: Port::create().expect("watched port"),
            badge,
        }
    }

    fn deliver(&self) -> OwnedHandle {
        self.port
            .handle()
            .duplicate(Rights::WRITE | Rights::TRANSFER, Some(self.badge))
            .expect("delivered capability minted")
    }

    fn assert_closed(&self) {
        kernel_tests::kassert!(matches!(
            self.port.recv(CLOSURE_TIMEOUT).err(),
            Some(Error::Syscall(SyscallError::SenderGone))
        ));
    }
}

/// Состав транзакции сверх объявления закрывает весь доставленный набор.
#[kernel_test]
fn a_set_past_the_declaration_closes_every_delivered_capability() {
    let log = Watched::create(LOG_BADGE);
    let control = Watched::create(CONTROL_BADGE);
    let service = Watched::create(SERVICE_BADGE);
    let first = Watched::create(FIRST_SLOT_BADGE);
    let second = Watched::create(SECOND_SLOT_BADGE);

    // Манифест фикстуры объявляет один слот, а транзакция содержит два.
    let _fixture = launch(|client| {
        client.start(log.deliver(), control.deliver(), service.deliver(), 2)?;
        client.slot(first.deliver())?;
        client.slot(second.deliver())
    });

    log.assert_closed();
    control.assert_closed();
    service.assert_closed();
    first.assert_closed();
    second.assert_closed();
}

/// Обрыв транзакции после базового кадра закрывает уже доставленный набор.
#[kernel_test]
fn a_transaction_torn_after_the_base_frame_closes_its_capabilities() {
    let log = Watched::create(LOG_BADGE);
    let control = Watched::create(CONTROL_BADGE);
    let service = Watched::create(SERVICE_BADGE);

    // Обещанный слот не приходит, и уход значка стартового порта обрывает приём компонента.
    let _fixture =
        launch(|client| client.start(log.deliver(), control.deliver(), service.deliver(), 1));

    log.assert_closed();
    control.assert_closed();
    service.assert_closed();
}

/// Кадр, отвергнутый приёмником, закрывает capability, которую транспорт с ним доставил.
#[kernel_test]
fn a_refused_frame_closes_the_capability_it_carried() {
    let slot = Watched::create(FIRST_SLOT_BADGE);

    // Слот до базового кадра приёмник отвергает.
    let _fixture = launch(|client| client.slot(slot.deliver()));

    slot.assert_closed();
}

fn launch(
    send: impl FnOnce(&component_startup::Client<PortTransport>) -> Result<(), IpcError>,
) -> Process {
    let mapping = image_mapping();
    let program = image_program(&mapping, FIXTURE_ENTRY);
    let resource = Resource::self_resource().expect("self metering resource");

    let InitializedProcess { process, startup } = launch_entry_retained(
        &program,
        Some(FIXTURE_NAME),
        &resource,
        STARTUP_LEASE_BADGE,
        move |startup| -> Result<(), ManagedStartupError> {
            let transport =
                PortTransport::client(startup.handle()).with_send_timeout(DELIVERY_TIMEOUT);
            send(&component_startup::Client::new(transport)).map_err(ManagedStartupError::from)
        },
    )
    .expect("startup transaction delivered");

    // Транзакция одноразовая, и уход значка стартового порта виден компоненту отказом
    // приёма. Так обрывается транзакция, оставленная без обещанного слота.
    drop(startup);
    process
}
