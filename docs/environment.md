# Окружение и зависимости

## Требования

### Rust toolchain

- Rust ≥ 1.85.0 (Edition 2024)
- `rust-toolchain.toml` **отсутствует** — тулчейн нужно настраивать вручную

```bash
rustup target add aarch64-unknown-none
rustup component add llvm-tools-preview
cargo install cargo-binutils
```

### Системные зависимости

- `qemu-system-arm` (предоставляет `qemu-system-aarch64`) — для end-to-end тестирования ядра

```bash
sudo apt-get install -y qemu-system-arm
```

## Подводные камни

### `cargo test` без `--exclude` падает

Крейты `drivers-aarch64`, `arch-aarch64` и `kernel` содержат aarch64 inline assembly, который не компилируется на x86_64. Всегда используйте:

```bash
cargo test --workspace --exclude drivers-aarch64 --exclude arch-aarch64 --exclude kernel
```

### QEMU работает бесконечно

Ядро входит в цикл обработки таймерных тиков после загрузки. В CI/автоматизации оборачивайте запуск в `timeout`:

```bash
timeout 10 qemu-system-aarch64 ...
```

### `Cargo.lock` в `.gitignore`

Зависимости разрешаются заново при каждой сборке. Это сделано намеренно — проект является ядром, а не библиотекой.

### Нет `rust-toolchain.toml`

Тулчейн нужно настраивать вручную. Убедитесь, что установлены:

- target `aarch64-unknown-none`
- компонент `llvm-tools-preview`
- `cargo-binutils`

## Спецификации устройств

Конфигурации хранятся в `devices/spec/*.yaml`:

```yaml
device:
  name: Device Name
  arch: aarch64

boot:
  format: binary | android_boot_v1 | android_boot_v2
  offset: 0x40200000
  dtb: /devices/dtb/device.dtb

run:
  - "qemu-system-aarch64 -machine virt -cpu cortex-a53 -m 512M -nographic -kernel target/build/kernel.bin"

debug:
  - "qemu-system-aarch64 -machine virt ... -S -gdb tcp::1234"
```

| Поле | Описание |
|---|---|
| `format` | `binary` (QEMU, RPi), `android_boot_v1`, `android_boot_v2` |
| `offset` | Адрес загрузки ядра (`KERNEL_OFFSET`) |
| `dtb` | Путь к Device Tree Blob (для Android) |
| `run` | Команды для `--run` |
| `debug` | Команды для `--debug` |
