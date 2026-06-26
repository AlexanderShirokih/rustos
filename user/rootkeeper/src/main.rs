#![no_std]
#![no_main]
#![allow(unsafe_code)]

use core::panic::PanicInfo;

use bootstrap::{BootstrapClient, LOG_MESSAGE_MAX};
use ipc::wire::Str;
use runtime::{MemoryRegion, OwnedHandle, PortTransport, UserMemFlags, thread_exit};
use syscall::Handle;
use userland_image::USERLAND_IMAGE_MAGIC;

/// `bootstrap_handle` приходит в x0 как сырой HandleId bootstrap-port'а
/// (клиент-отправитель лога), переданного ядром при спавне.
#[unsafe(no_mangle)]
pub extern "C" fn _start(bootstrap_handle: usize) -> ! {
    let bootstrap = Handle::new(bootstrap_handle as u32).expect("bootstrap handle is non-zero");
    let client = BootstrapClient::new(PortTransport::client(bootstrap));
    if client
        .log(Str::<LOG_MESSAGE_MAX>::new("rootkeeper started").expect("startup log must fit"))
        .is_err()
    {
        thread_exit(1)
    }

    thread_exit(u64::from(!image_carries_magic(&client)))
}

/// Запрашивает по bootstrap-контракту read-only регион userland-образа, маппит
/// его и сверяет первые байты с [`USERLAND_IMAGE_MAGIC`]. `true` - магия видна.
fn image_carries_magic(client: &BootstrapClient<PortTransport>) -> bool {
    let Ok(Ok(cap)) = client.acquire_userland_image() else {
        return false;
    };
    let region = MemoryRegion::from_handle(OwnedHandle::adopt(cap));

    let Ok(info) = region.inspect() else {
        return false;
    };
    let Ok(mapping) = region.map(info.size_bytes, UserMemFlags::ReadOnly) else {
        return false;
    };

    let mut magic = [0u8; USERLAND_IMAGE_MAGIC.len()];
    if mapping.size_bytes() < magic.len() as u64 {
        return false;
    }
    let base = mapping.va() as *const u8;
    for (i, slot) in magic.iter_mut().enumerate() {
        // SAFETY: маппинг покрывает size_bytes >= magic.len() байт read-only памяти
        // образа; читаем in-bounds по сырому указателю на живой маппинг.
        *slot = unsafe { base.add(i).read_volatile() };
    }
    magic == USERLAND_IMAGE_MAGIC
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
