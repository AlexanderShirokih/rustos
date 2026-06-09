# Обзор проекта

RustOS Mobile — bare-metal ядро для aarch64 на Rust (`#![no_std]`,
Edition 2024). Workspace состоит из платформенного слоя, ядра,
архитектурно-независимых подсистем, драйверов и host-инструмента `xtask`.

## Целевые платформы

- QEMU `virt` — основная среда разработки и integration-тестов.
- Raspberry Pi 5 — `linux_arm64` image.
- Xiaomi Redmi Note 7 — Android boot image v1.

## Основные слои

| Слой                 | Крейты                                                                |
|----------------------|-----------------------------------------------------------------------|
| Boot/platform        | `hal-aarch64`, `hal-common`, `hal-aarch64-asid`, `hal-aarch64-paging` |
| Kernel orchestration | `kernelspace`                                                         |
| Runtime ядра         | `scheduler`, `kobject`, `syscall`, `userspace`, `memory`              |
| Драйверы             | `drivers-aarch64`, `drivers-common`, `drivers-common-aarch64`         |
| Базовые библиотеки   | `collections`, `fdt`, `io`, `klog`, `util`                            |
| Тестовый harness     | `test-harness-qemu`, `test-harness-qemu-aarch64`                      |
| Host tooling         | `xtask`                                                               |

Зависимости идут от платформенного и orchestration-кода к нижним
подсистемам. Архитектурно-независимые крейты не должны зависеть от
`hal-aarch64` или платформенных драйверов.
