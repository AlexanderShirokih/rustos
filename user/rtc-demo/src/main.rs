#![no_std]
#![no_main]
#![allow(unsafe_code)]

extern crate alloc;

use core::{num::NonZeroU64, panic::PanicInfo};

use bootstrap::BootstrapClient;
use fdt::{devicetree::DeviceTree, devicetreeext::NodeExt};
use log_relay::{KernelLogSink, LogRelayThread};
use rtc_demo::{ClientArgsClient, DriverArgsClient, Error, Result};
use runtime::{
    MemoryRegion, OwnedHandle, Port, PortTransport, Process, Resource, Timeout, UserMemFlags,
    thread_exit,
};
use syscall::Handle;
use userland_image::{DecodedImage, decode};
use userland_loader::launch_entry;

// Значок clock-сессии клиента у драйвера.
const CLOCK_SESSION_BADGE: NonZeroU64 = NonZeroU64::new(1).expect("badge is non-zero");

// Значок лог-сессии клиента часов.
const CLIENT_LOG_BADGE: NonZeroU64 = NonZeroU64::new(2).expect("badge is non-zero");

fn log_source_name(badge: u64) -> Option<&'static str> {
    (badge == CLIENT_LOG_BADGE.get()).then_some("rtc-client")
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(root: Handle) -> ! {
    thread_exit(u64::from(run(root).is_err()))
}

fn run(root: Handle) -> Result<()> {
    let root = OwnedHandle::from_handle(root);
    let kernel = BootstrapClient::new(PortTransport::client(root.borrow()));
    let relay = LogRelayThread::start(log_source_name, KernelLogSink::new(root.as_raw()))?;

    let outcome = run_clock(&kernel, &relay);
    // Закрытие лог-порта отзывает сессию клиента и завершает поток relay: код
    // процесса берёт последний вышедший поток, и им обязан быть этот.
    relay.shutdown(Timeout::INFINITE)?;
    outcome
}

fn run_clock(kernel: &BootstrapClient<PortTransport>, relay: &LogRelayThread) -> Result<()> {
    let resource = Resource::self_resource()?;

    // Образ для спавна и device-окно PL031 от ядра.
    let image_region =
        MemoryRegion::adopt(kernel.acquire_userland_image()?.map_err(|_| Error::Vend)?);
    let image_mapping = image_region.map_full(UserMemFlags::ReadOnly)?;
    // SAFETY: регион образа ядро выдаёт только на чтение: отображений на запись
    // у него нет ни в одном адресном пространстве.
    let image = decode(unsafe { image_mapping.as_bytes() })?;
    let base = get_pl031_base(kernel)?;
    let device: MemoryRegion = kernel
        .acquire_device_memory_by_base(base)?
        .ok_or(Error::NoDevice)?;

    let clock_port = Port::create_service(Some(CLOCK_SESSION_BADGE))?;

    let driver = launch(
        &image,
        "/boot/rtc-driver",
        &resource,
        |startup| -> Result<()> {
            DriverArgsClient::new(PortTransport::client(startup.handle()))
                .provide(device.into_handle(), clock_port.server)?;
            Ok(())
        },
    )?;

    let client = launch(
        &image,
        "/boot/rtc-client",
        &resource,
        |startup| -> Result<()> {
            ClientArgsClient::new(PortTransport::client(startup.handle())).provide(
                clock_port.client.into_handle(),
                relay.session(CLIENT_LOG_BADGE)?,
            )?;
            Ok(())
        },
    )?;

    // Клиент уносит значок clock-сессии, поэтому его уход завершает и драйвер.
    client.join(Timeout::INFINITE)?;
    driver.join(Timeout::INFINITE)?;
    Ok(())
}

fn launch(
    image: &DecodedImage,
    path: &str,
    resource: &Resource,
    startup: impl FnOnce(&Port) -> Result<()>,
) -> Result<Process> {
    let program = image.entry(path).ok_or(Error::MissingEntry)?.program()?;
    launch_entry(&program, resource, startup)
}

fn get_pl031_base(kernel: &BootstrapClient<PortTransport>) -> Result<u64> {
    let fdt_region = MemoryRegion::adopt(kernel.acquire_boot_fdt()?.map_err(|_| Error::Vend)?);
    let fdt_mapping = fdt_region.map_full(UserMemFlags::ReadOnly)?;
    // SAFETY: регион DTB ядро выдаёт только на чтение: отображений на запись у
    // него нет ни в одном адресном пространстве.
    let fdt_bytes = unsafe { fdt_mapping.as_bytes() };
    let tree = DeviceTree::from_bytes(fdt_bytes).map_err(|_| Error::Dtb)?;
    let root = tree.root().ok_or(Error::Dtb)?;
    let pl031 = root
        .children()
        .find(|node| node.is_compatible("arm,pl031"))
        .ok_or(Error::Dtb)?;
    pl031.first_reg_base(&root).ok_or(Error::Dtb)
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
