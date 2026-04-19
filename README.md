# RustOS Mobile - Экспериментальное ядро мобильной ОС на Rust

Проект по созданию минималистичного ядра операционной системы для мобильных устройств с нуля на языке Rust.

**Целевое устройство**: Проект тестирвоался на Xiaomi Redmi Note 7 (lavender, Qualcomm SDM660), Raspberry Pi 5, QEMU. Вы можете попробовать собрать для другой модели телефона, однако вам придется доработать код самостоятельно
**Архитектура**: ARMv8-A (aarch64)

Инструкции по требованиям, сборке и запуску: **[BUILD.md](BUILD.md)**.

## Прогон тестов
- `cargo test`

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

## Дальнейшие планы

- [ ] Поддержка многозадачности
- [ ] Драйверы для периферии (GPIO, I2C, SPI)
- [ ] Файловая система
- [ ] Поддержка других устройств

## Полезные ресурсы

- [Device Tree Specification](https://www.devicetree.org/)
- [OSDev Wiki](https://wiki.osdev.org/)
- [Статья на Хабре о проекте](https://habr.com/ru/articles/962680/)

## Лицензия

Проект распространяется "как есть" для образовательных целей.
