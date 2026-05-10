# Окружение и зависимости

## Rust toolchain

Toolchain зафиксирован в `rust-toolchain.toml`:

- channel: `nightly`
- target: `aarch64-unknown-none`
- components: `rust-src`, `llvm-tools-preview`, `clippy`, `rustfmt`

`rustup` подхватывает toolchain автоматически при запуске `cargo`.

## Host-зависимости

| Инструмент | Когда нужен |
|---|---|
| `cargo-binutils` | `cargo objcopy` внутри `xtask build` |
| `qemu-system-aarch64` | запуск QEMU и QEMU integration tests |
| `mkbootimg` | сборка `android_boot_v1` / `android_boot_v2` |
| `cargo-deny` | `cargo deny check` |

Минимальная установка для QEMU-разработки:

```bash
rustup component add llvm-tools-preview
cargo install cargo-binutils
sudo apt-get install -y qemu-system-arm
```

Для Android boot image дополнительно нужен `mkbootimg`.

## Device specs

Спеки лежат в `devices/spec/*.yaml`.

```yaml
device:
  name: QEMU AArch64
  arch: aarch64

boot:
  format: linux_arm64
  offset: 0x40200000
  dtb: /devices/dtb/device.dtb
  base: 0x40000000

run:
  - "qemu-system-aarch64 ..."

debug:
  - "qemu-system-aarch64 ... -S -gdb tcp::1234"
```

| Поле | Описание |
|---|---|
| `device.arch` | сейчас поддерживается `aarch64` |
| `boot.format` | `linux_arm64`, `android_boot_v1`, `android_boot_v2`; `uefi` зарезервирован |
| `boot.offset` | link/load offset ядра (`KERNEL_OFFSET`) |
| `boot.dtb` | DTB для Android boot image |
| `boot.base` | база для расчёта Android `kernel_offset` |
| `run` | команды для `xtask build --run` |
| `debug` | команды для `xtask build --debug` |
