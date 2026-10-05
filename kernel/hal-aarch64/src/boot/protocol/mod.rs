#[cfg(all(feature = "boot-linux-arm64", feature = "boot-uefi"))]
compile_error!("features `boot-linux-arm64` and `boot-uefi` are mutually exclusive");

#[cfg(not(any(feature = "boot-linux-arm64", feature = "boot-uefi")))]
compile_error!("one of `boot-linux-arm64` or `boot-uefi` must be enabled");

#[cfg(feature = "boot-linux-arm64")]
pub(crate) mod linux_arm64;
