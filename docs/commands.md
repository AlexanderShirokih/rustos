# Команды

## Быстрая справка

| Действие               | Команда                                                                                                                      |
|------------------------|------------------------------------------------------------------------------------------------------------------------------|
| Host-тесты             | `cargo test --workspace`                                                                                                     |
| Host-clippy            | `cargo clippy --workspace`                                                                                                   |
| AArch64 clippy         | `cargo clippy --workspace --exclude xtask --exclude userland-image-tool --exclude ipc-test --target aarch64-unknown-none` |
| Форматирование         | `cargo fmt --all --check`                                                                                                    |
| Сборка userland        | `cargo xtask build-userland [--image <имя>]`                                                                                 |
| Сборка QEMU            | `cargo xtask build devices/spec/qemu-aarch64.yaml`                                                                           |
| QEMU integration tests | `cargo xtask qemu-test --timeout 20`                                                                                         |
| Сборка устройства      | `cargo xtask build devices/spec/<device>.yaml`                                                                               |
| Проверка слоёв         | `cargo xtask check-layers`                                                                                                   |

## Сборка

`xtask build` читает YAML-спеку устройства, сначала собирает
`target/build/userland.img` из `/user/images/default.toml`, затем выбирает boot
feature для `hal-aarch64`, собирает kernel crate под `aarch64-unknown-none` и
упаковывает результат.

Боевой kernel-бинарь `kernel-aarch64` (как и userland-бинари) задаёт
`forced-target = "aarch64-unknown-none"`: крейт всегда собирается под bare-metal,
независимо от наличия `--target`. Это нужно из-за boot-asm с ELF-релокациями,
которые host-ассемблер не принимает; `forced-target` снимает необходимость
гейтить код или bin под host-сборку.

```bash
cargo xtask build devices/spec/qemu-aarch64.yaml
cargo xtask build devices/spec/xiaomi-lavender.yaml
```

`xtask build-userland` собирает только `target/build/userland.img`. Флаг
`--image <имя>` выбирает композицию `user/images/<имя>.toml`; по умолчанию —
`default`.

```bash
cargo xtask build-userland
cargo xtask build-userland --image test
```

Результаты:

| `boot.format`     | Артефакт                                                                                           |
|-------------------|----------------------------------------------------------------------------------------------------|
| всегда            | `target/build/userland.img`                                                                        |
| `linux_arm64`     | `target/build/kernel.bin` + sidecar `target/build/userland.img`, который нужно передать как initrd |
| `android_boot_v1` | `target/build/boot.img` с `userland.img` в ramdisk                                                 |
| `android_boot_v2` | `target/build/boot.img` с `userland.img` в ramdisk                                                 |
| `uefi`            | не реализован                                                                                      |

## Тесты

```bash
cargo test --workspace
```

QEMU integration tests:

```bash
cargo xtask qemu-test --timeout 20
```

`qemu-test` собирает `target/build/userland.img` и запускает QEMU с `-initrd target/build/userland.img`.

## Проверка слоёв

```bash
cargo xtask check-layers
```

Сверяет рёбра зависимостей между workspace-крейтами (по `cargo metadata`)
с правилами слоёв из [`architecture.md`](architecture.md).
При нарушениях печатает запрещённые рёбра и завершается с ошибкой; выполняется в CI.
