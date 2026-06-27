#![no_std]
#![no_main]
#![allow(unsafe_code)]

use core::panic::PanicInfo;

use bootstrap::BootstrapClient;
use ipc::wire::FieldStr;
use runtime::{MemoryRegion, OwnedHandle, PortTransport, UserMemFlags, thread_exit};
use syscall::Handle;
use userland_image::USERLAND_IMAGE_MAGIC;

/// `bootstrap` приходит в x0 как HandleId bootstrap Port, переданного ядром.
#[unsafe(no_mangle)]
pub extern "C" fn _start(bootstrap: Handle) -> ! {
    let client = BootstrapClient::new(PortTransport::client(bootstrap));
    let _ = client.log(FieldStr::new("rootkeeper started").expect("within bound"));

    thread_exit(u64::from(!image_carries_magic(&client)))
}

/// Запрашивает по bootstrap-контракту read-only регион userland-образа, маппит
/// его и сверяет первые байты с [`USERLAND_IMAGE_MAGIC`].
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
