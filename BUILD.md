# Сборка RustOS Mobile

## Требования

- Rust toolchain из `rust-toolchain.toml` (nightly)
- `cargo-binutils` — для `cargo objcopy`
- `qemu-system-aarch64` — для запуска QEMU
- `mkbootimg` — для создания Android boot image

```bash
rustup component add llvm-tools-preview
cargo install cargo-binutils
```

## Сборка

Сборка выполняется через `cargo xtask` с указанием спецификации устройства:

```bash
# Userland image
cargo xtask build-userland

# QEMU (формат linux_arm64)
cargo xtask build devices/spec/qemu-aarch64.yaml

# Xiaomi Redmi Note 7 (формат android_boot_v1)
cargo xtask build devices/spec/xiaomi-lavender.yaml
```

### Результат сборки

- всегда — `target/build/userland.img`
- `linux_arm64` — `target/build/kernel.bin` + sidecar `target/build/userland.img` (загрузчик должен передать sidecar как initrd)
- `android_boot_v1`, `android_boot_v2` — `target/build/boot.img` с `userland.img` внутри ramdisk
- `uefi` — WIP, пока не реализовано

## Запуск

```bash
# Сборка + запуск (выполняет команды из 'run' в YAML)
cargo xtask build devices/spec/qemu-aarch64.yaml --run

# Сборка + отладка (выполняет команды из 'debug' в YAML)
cargo xtask build devices/spec/qemu-aarch64.yaml --debug

# QEMU integration tests
cargo xtask qemu-test --timeout 20
```
