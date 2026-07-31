# Команды

## Быстрая справка

| Действие                            | Команда                                                                                                              |
|-------------------------------------|----------------------------------------------------------------------------------------------------------------------|
| Host-тесты                          | `cargo test --workspace`                                                                                             |
| Host-clippy                         | `cargo clippy --workspace`                                                                                           |
| AArch64 clippy                      | `cargo clippy --workspace --exclude xtask --exclude userland-build --exclude ipc-test --target aarch64-unknown-none` |
| Форматирование                      | `cargo fmt --all --check`                                                                                            |
| Проверка слоёв                      | `cargo xtask check-layers`                                                                                           |
| Сборка userland                     | `cargo xtask build-userland [--image <путь>]`                                                                        |
| Загрузочный образ (userland + ядро) | `cargo xtask build devices/spec/<device>.yaml [--image <путь>]`                                                      |
| Загрузочный образ для QEMU          | `cargo xtask build devices/spec/qemu-aarch64.yaml`                                                                   |
| Запуск после сборки                 | `cargo xtask build devices/spec/<device>.yaml --run`                                                                 |
| QEMU integration tests              | `cargo xtask qemu-test [--timeout <сек>]`                                                                            |
| Userspace stress в QEMU             | `cargo xtask stress [--seed <n>] [--seeds <n>] [--steps <n>] [--timeout <сек>]`                                      |
| Host race под ThreadSanitizer       | `cargo xtask race`                                                                                                    |
| Ограниченный fuzz-прогон            | `cargo xtask fuzz <target> [--seconds <сек>]`                                                                         |

## Сборка

```
cargo xtask build <spec> [--run] [--debug] [--features <features>] [--image <путь>]
```

Читает YAML-спеку устройства, собирает `target/build/userland.img`, затем собирает `hal-aarch64` под
`aarch64-unknown-none --release` и упаковывает результат по правилам `boot.format`.

Флаги:

| Флаг                  | Назначение                                                                                     |
|-----------------------|------------------------------------------------------------------------------------------------|
| `--run`               | После сборки выполнить команды из секции `run` YAML-спеки                                      |
| `--debug`             | После сборки выполнить команды из секции `debug` YAML-спеки                                    |
| `--features <список>` | Дополнительные features (через запятую)                                                        |
| `--image <путь>`      | Относительный путь к TOML-манифесту userland-образа. По умолчанию `user/rootkeeper/image.toml` |

```bash
cargo xtask build devices/spec/xiaomi-lavender.yaml --image user/calculator-demo/image.toml
```

Артефакты по формату загрузчика:

| `boot.format`     | Артефакт                                                                                      |
|-------------------|-----------------------------------------------------------------------------------------------|
| всегда            | `target/build/userland.img`                                                                   |
| `linux_arm64`     | `target/build/kernel.bin` + `target/build/userland.img` (initrd)                              |
| `android_boot_v1` | `target/build/boot.img` с `userland.img` в ramdisk (DTB конкатенирован с kernel.gz)           |
| `android_boot_v2` | `target/build/boot.img` с `userland.img` в ramdisk (DTB передаётся отдельным полем заголовка) |
| `uefi`            | не реализован                                                                                 |

## Userland

```
cargo xtask build-userland [--image <путь>]
```

Собирает только `target/build/userland.img`. Флаг `--image` задаёт относительный путь к
TOML-манифесту композиции. По умолчанию — `user/rootkeeper/image.toml`.

```bash
cargo xtask build-userland
cargo xtask build-userland --image user/testrunner/image.toml
```

## Тесты

Host-тесты:

```bash
cargo test --workspace
```

QEMU integration tests:

```bash
cargo xtask qemu-test [--timeout <сек>]
```

По умолчанию тайм-аут — 60 секунд. `qemu-test` выполняет проход по спецификации `devices/spec/qemu-aarch64-test.yaml`:

| Проход     | Feature ядра    | Userland-образ                | Что тестирует     |
|------------|-----------------|-------------------------------|-------------------|
| `kernel`   | `kernel-tests`  | `user/rootkeeper/image.toml`  | тесты внутри ядра |
| `userland` | —               | `user/testrunner/image.toml`  | тесты в userspace |

Ядро пересобирается между прогонами из-за различия feature-флагов.
Выбор прохода для нового теста — в [`code-style.md`](code-style.md).

### Stress, race и fuzz

`stress` собирает отдельный образ testrunner, выбирает только
`#[kernel_test(stress)]` и повторяет одну последовательность операций для
диапазона seed.

```bash
cargo xtask stress --seed 100 --seeds 32 --steps 256 --timeout 900
```

При сбое выводятся seed, номер операции и её имя. Тем же seed и числом шагов
повторяется последовательность операций, но не их чередование с ядром: QEMU
выполняется с одним vCPU, и этот слой проверяет границы жизненного цикла и
ожиданий, оставляя межъядерную память слою `race`.

`race` запускает host-only тесты низкоуровневых примитивов с ThreadSanitizer:

```bash
cargo xtask race
```

Fuzz-targets лежат в отдельном workspace `fuzz/` — он собирается своим
toolchain и не меняет target-конфигурацию основного bare-metal workspace:

```bash
cargo xtask fuzz fdt_parse_walk --seconds 60
cargo xtask fuzz ipc_wire_decode --seconds 60
cargo xtask fuzz userland_image_decode --seconds 60
```

Корпус находится в `fuzz/corpus/<target>/`, crash-артефакты — в
`fuzz/artifacts/<target>/` и не добавляются в Git автоматически. Для прямого
запуска доступны также `cargo fuzz build --fuzz-dir fuzz` и `cargo fuzz run
--fuzz-dir fuzz <target> -- <libFuzzer args>`.

## Проверка слоёв

```bash
cargo xtask check-layers
```

Сверяет рёбра зависимостей между workspace-крейтами (по `cargo metadata`)
с правилами слоёв из [`architecture.md`](architecture.md).
При нарушениях печатает запрещённые рёбра и завершается с ошибкой.
