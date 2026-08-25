//! Живость control-порта компонента: терминален уход значка супервизора, а уход прочих
//! отправителей порта обслуживание продолжает.

use core::num::NonZeroU64;

use component_abi::{CONTROL_LEASE_BADGE, component_sessions, component_startup};
use kernel_tests::kernel_test;
use managed_component::ManagedStartupError;
use runtime::{OwnedHandle, Port, PortTransport, Resource, Timeout};
use syscall::Rights;
use userland_loader::launch_entry;

use crate::{
    filesystem::whole_export,
    managed_startup::{image_mapping, image_program},
};

const PROVIDER_ENTRY: &str = "/boot/spare-filesystem/program";
const PROVIDER_NAME: &str = "spare-filesystem";

const FILE_SYSTEM_SUPERVISOR_LOST_EXIT_CODE: u32 = 2;

const GUEST_BADGE: NonZeroU64 = NonZeroU64::new(0x51).expect("badge is non-zero");
const LOG_BADGE: NonZeroU64 = NonZeroU64::new(0x52).expect("badge is non-zero");

const DELIVERY_TIMEOUT: Timeout = Timeout::from_secs(1);
const CALL_TIMEOUT: Timeout = Timeout::from_secs(5);
const PROVIDER_TURN: Timeout = Timeout::from_millis(200);
const PROCESS_TIMEOUT: Timeout = Timeout::from_secs(5);

#[kernel_test]
fn a_control_port_outlives_every_sender_but_the_supervisor_lease() {
    let mapping = image_mapping();
    let program = image_program(&mapping, PROVIDER_ENTRY);
    let resource = Resource::self_resource().expect("self metering resource");

    let control = Port::create().expect("control port");
    let service = Port::create().expect("service port");
    let minted = Rights::WRITE | Rights::TRANSFER;
    // Оба значка выведены из незаклеймённого корня: заклеймённая капа второго значка
    // не принимает.
    let lease = control
        .handle()
        .duplicate(minted, Some(CONTROL_LEASE_BADGE))
        .expect("supervisor lease minted");
    let guest = control
        .handle()
        .duplicate(minted, Some(GUEST_BADGE))
        .expect("control import minted");
    let log = crate::log_relay::component_log_session(LOG_BADGE).expect("log session minted");
    let control = control.into_handle();
    let service = service.into_handle();

    let process = launch_entry(
        &program,
        Some(PROVIDER_NAME),
        &resource,
        move |startup| -> Result<(), ManagedStartupError> {
            let transport =
                PortTransport::client(startup.handle()).with_send_timeout(DELIVERY_TIMEOUT);
            component_startup::Client::new(transport)
                .start(log, control, service, 0)
                .map_err(ManagedStartupError::from)
        },
    )
    .expect("startup frame delivered");

    // Провайдер обслуживает control-порт значку импорта: ответ ставит его цикл приёма в
    // известное состояние до ухода значка.
    let session = component_sessions::Client::new(PortTransport::client(&guest))
        .with_wait(CALL_TIMEOUT)
        .open_session(whole_export())
        .expect("control port answers the import")
        .expect("session established");
    drop(OwnedHandle::adopt(session));

    drop(guest);
    // Уход значка импорта провайдера не завершает.
    kernel_tests::kassert!(process.join(PROVIDER_TURN).is_err());

    drop(lease);
    process
        .join(PROCESS_TIMEOUT)
        .expect("supervisor departure stops the provider");
    kernel_tests::kassert_eq!(
        process.exit_code(),
        Ok(FILE_SYSTEM_SUPERVISOR_LOST_EXIT_CODE)
    );
}
