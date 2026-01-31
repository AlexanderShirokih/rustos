# Mobile Kernel

Ядро для мобильных устройств на архитектуре AArch64.

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
# QEMU (формат binary)
cargo xtask build devices/spec/qemu-aarch64.yaml

# Xiaomi Redmi Note 7 (формат android_boot_v1)
cargo xtask build devices/spec/xiaomi-lavender.yaml
```

### Результат сборки

- `binary` — `target/build/kernel.bin`
- `android_boot_v1`, `android_boot_v2` — `target/build/boot.img`

## Запуск

```bash
# Сборка + запуск (выполняет команды из 'run' в YAML)
cargo xtask build devices/spec/qemu-aarch64.yaml --run

# Сборка + отладка (выполняет команды из 'debug' в YAML)
cargo xtask build devices/spec/qemu-aarch64.yaml --debug
```

## Спецификации устройств

Конфигурации устройств хранятся в `devices/spec/*.yaml`:

```yaml
device:
  name: Device Name
  arch: aarch64

boot:
  format: binary | android_boot_v1 | android_boot_v2
  offset: 0x40200000
  dtb: /devices/dtb/device.dtb  # для android_boot_*

run:
  - "qemu-system-aarch64 -machine virt -cpu cortex-a53 -m 512M -nographic -kernel target/build/kernel.bin"

debug:
  - "qemu-system-aarch64 -machine virt ... -S -gdb tcp::1234"
```

- `format`:
  - `binary` — простой бинарник (QEMU, RPi)
  - `android_boot_v1` — boot.img с appended DTB, header version 1
  - `android_boot_v2` — boot.img с отдельным DTB, header version 2
- `offset` — адрес загрузки ядра (KERNEL_OFFSET)
- `dtb` — путь к Device Tree Blob (для Android)
- `run` — команды для запуска (`--run`)
- `debug` — команды для отладки (`--debug`)
