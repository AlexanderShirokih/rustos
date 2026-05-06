# Сборка RustOS Mobile

## Требования

- Rust (stable)
- `cargo-binutils` — для `cargo objcopy`
- `mkbootimg` — для создания Android boot image

```bash
rustup component add llvm-tools-preview
cargo install cargo-binutils
```

## Сборка

Сборка выполняется через `cargo xtask` с указанием спецификации устройства:

```bash
# QEMU (формат linux_arm64)
cargo xtask build devices/spec/qemu-aarch64.yaml

# Xiaomi Redmi Note 7 (формат android_boot_v1)
cargo xtask build devices/spec/xiaomi-lavender.yaml
```

### Результат сборки

- `linux_arm64` — `target/build/kernel.bin`
- `android_boot_v1`, `android_boot_v2` — `target/build/boot.img`
- `uefi` — WIP, пока не реализовано

## Запуск

```bash
# Сборка + запуск (выполняет команды из 'run' в YAML)
cargo xtask build devices/spec/qemu-aarch64.yaml --run

# Сборка + отладка (выполняет команды из 'debug' в YAML)
cargo xtask build devices/spec/qemu-aarch64.yaml --debug
```
