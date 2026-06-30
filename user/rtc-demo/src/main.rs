#![no_std]
#![no_main]
#![allow(unsafe_code)]

extern crate alloc;

use core::panic::PanicInfo;

use bootstrap::BootstrapClient;
use fdt::{devicetree::DeviceTree, devicetreeext::NodeExt};
use rtc_demo::{ClientArgsClient, DriverArgsClient, Error, Result};
use runtime::{
    MemoryRegion, OwnedHandle, Port, PortTransport, Process, Resource, Timeout, UserMemFlags,
    thread_exit,
};
use syscall::{Handle, Rights};
use userland_image::{DecodedImage, decode};
use userland_loader::load_entry;

#[unsafe(no_mangle)]
pub extern "C" fn _start(root: Handle) -> ! {
    thread_exit(u64::from(run(root).is_err()))
}

fn run(root: Handle) -> Result<()> {
    let root = OwnedHandle::from_handle(root);
    let kernel = BootstrapClient::new(PortTransport::client(root.borrow()));
    let resource = Resource::self_resource();

    // Образ для спавна и device-окно PL031 от ядра.
    let image_region =
        MemoryRegion::adopt(kernel.acquire_userland_image()?.map_err(|_| Error::Vend)?);
    let image_mapping = image_region.map_full(UserMemFlags::ReadOnly)?;
    let image = decode(image_mapping.as_bytes())?;
    let base = get_pl031_base(&kernel)?;
    let device: MemoryRegion = kernel
        .acquire_device_memory_by_base(base)?
        .ok_or(Error::NoDevice)?;

    // Clock-порт связывает драйвер и клиента; args-порты доставляют каждому
    // его стартовые ресурсы.
    let clock_port = Port::create()?;
    let driver_args_port = Port::create()?;
    let client_args_port = Port::create()?;

    // Драйвер стартует с серверной частью своего args-порта.
    let driver = spawn(
        &image,
        "rtc-driver",
        &resource,
        driver_args_port
            .handle()
            .duplicate(Rights::READ | Rights::WRITE | Rights::TRANSFER, 0)?,
    )?;

    let driver_args = DriverArgsClient::new(PortTransport::client(driver_args_port.handle()));
    driver_args.provide(
        device.into_handle(),
        clock_port
            .handle()
            .duplicate(Rights::READ | Rights::WRITE | Rights::TRANSFER, 0)?,
    )?;

    // Клиент - симметрично: клиентская часть clock-порта и write-копия klog.
    let client = spawn(
        &image,
        "rtc-client",
        &resource,
        client_args_port
            .handle()
            .duplicate(Rights::READ | Rights::WRITE | Rights::TRANSFER, 0)?,
    )?;

    let client_args = ClientArgsClient::new(PortTransport::client(client_args_port.handle()));
    client_args.provide(
        clock_port
            .handle()
            .duplicate(Rights::WRITE | Rights::TRANSFER, 0)?,
        root.duplicate(Rights::WRITE | Rights::TRANSFER, 0)?,
    )?;

    let _ = client.join(Timeout::INFINITE);

    drop(driver);
    Ok(())
}

fn spawn(
    image: &DecodedImage,
    name: &str,
    resource: &Resource,
    startup: OwnedHandle,
) -> Result<Process> {
    let entry = image.entry(name).ok_or(Error::MissingEntry)?;
    Ok(load_entry(&entry, resource, startup)?)
}

fn get_pl031_base(kernel: &BootstrapClient<PortTransport>) -> Result<u64> {
    let fdt_region = MemoryRegion::adopt(kernel.acquire_boot_fdt()?.map_err(|_| Error::Vend)?);
    let fdt_mapping = fdt_region.map_full(UserMemFlags::ReadOnly)?;
    let tree = DeviceTree::from_bytes(fdt_mapping.as_bytes()).map_err(|_| Error::Dtb)?;
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
