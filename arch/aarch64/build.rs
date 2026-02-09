use std::env;
use std::fs::File;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct DeviceSection {
    name: String,
    arch: String,
}

#[derive(Debug, Deserialize)]
struct BootSection {
    format: String,
    offset: u64,
    #[serde(default)]
    dtb: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeviceSpec {
    device: DeviceSection,
    boot: BootSection,
}

fn main() {
    let manifest_dir =
        env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by Cargo");

    let project_root = Path::new(&manifest_dir)
        .join("../..")
        .canonicalize()
        .expect("failed to resolve project root");

    let spec_rel =
        env::var("DEVICE_SPEC").unwrap_or_else(|_| "devices/spec/qemu-aarch64.yaml".to_string());

    let spec_path = resolve_path(&project_root, &spec_rel);

    println!("cargo:rerun-if-env-changed=DEVICE_SPEC");
    println!("cargo:rerun-if-changed={}", spec_path.display());

    let file = File::open(&spec_path).unwrap_or_else(|e| {
        panic!(
            "failed to open device spec YAML at '{}': {e}",
            spec_path.display()
        )
    });

    let spec: DeviceSpec = serde_yaml::from_reader(file).unwrap_or_else(|e| {
        panic!(
            "failed to parse device spec YAML at '{}': {e}",
            spec_path.display()
        )
    });

    if spec.device.arch != "aarch64" {
        panic!(
            "device spec '{}' has arch='{}', but this crate targets 'aarch64'",
            spec.device.name, spec.device.arch
        );
    }

    let boot_format = spec.boot.format.as_str();
    match boot_format {
        "binary" | "android_boot_v1" | "android_boot_v2" => {}
        _ => panic!(
            "Unknown boot.format '{}'. Expected: binary, android_boot_v1, android_boot_v2",
            boot_format
        ),
    }

    // Передаём смещение ядра в линкер (-T должен идти перед --defsym)
    println!(
        "cargo:rustc-link-arg=--defsym=KERNEL_OFFSET={:#x}",
        spec.boot.offset
    );
    println!("cargo:rustc-link-arg=-Tarch/aarch64/linker/aarch64.ld");

    // Экспорт параметров в env для использования в коде и Makefile
    println!("cargo:rustc-env=BOOT_FORMAT={}", boot_format);

    if let Some(ref dtb) = spec.boot.dtb {
        println!("cargo:rustc-env=BOOT_DTB={}", dtb);
    }
}

fn resolve_path(base: &Path, spec_rel: &str) -> PathBuf {
    let candidate = Path::new(spec_rel);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base.join(candidate)
    }
}
