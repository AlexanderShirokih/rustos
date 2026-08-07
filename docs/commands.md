# Команды

## Быстрая справка

| Действие                            | Команда                                                                                                              |
|-------------------------------------|----------------------------------------------------------------------------------------------------------------------|
| Host-тесты                          | `cargo test --workspace`                                                                                             |
| Host-clippy                         | `cargo clippy --workspace`                                                                                           |
| AArch64 clippy                      | `cargo clippy --workspace --exclude xtask --exclude userland-build --exclude ipc-test --exclude style-tool --target aarch64-unknown-none` |
| Форматирование                      | `cargo fmt --all --check`                                                                                            |
| Проверка слоёв                      | `cargo xtask check-layers`                                                                                           |
| Проверка кодстайла                  | `cargo xtask style [--all] [--staged] [--since <rev>]`                                                               |
| Сборка userland                     | `cargo xtask build-userland [--image <путь>] [--file <путь>=<источник>]`                                             |
| Загрузочный образ (userland + ядро) | `cargo xtask build devices/spec/<device>.yaml [--image <путь>] [--file <путь>=<источник>]`                           |
| Загрузочный образ для QEMU          | `cargo xtask build devices/spec/qemu-aarch64.yaml`                                                                   |
| Запуск после сборки                 | `cargo xtask build devices/spec/<device>.yaml --run`                                                                 |
| QEMU integration tests              | `cargo xtask qemu-test [--timeout <сек>] [--spec <путь>]`                                                            |
| Userspace stress в QEMU             | `cargo xtask stress [--seed <n>] [--seeds <n>] [--steps <n>] [--timeout <сек>]`                                      |
| Host race под ThreadSanitizer       | `cargo xtask race`                                                                                                    |
| Ограниченный fuzz-прогон            | `cargo xtask fuzz <target> [--seconds <сек>]`                                                                         |

## Сборка

```
cargo xtask build <spec> [--run] [--debug] [--features <features>] [--image <путь>] [--file <путь>=<источник>]
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
| `--file <путь>=<источник>` | Файлы образа поверх манифеста: путь записи и источник относительно корня проекта, подстановки включая. Повторяется по числу источников |

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
cargo xtask build-userland [--image <путь>] [--file <путь>=<источник>]
```

Собирает только `target/build/userland.img`. Флаг `--image` задаёт относительный путь к
TOML-манифесту композиции. По умолчанию — `user/rootkeeper/image.toml`.

Флаг `--file` добавляет в образ файл поверх манифеста: слева путь записи, справа источник
относительно корня проекта. Источник принимает подстановки (`assets/**/*.txt`), и тогда путь
записи служит префиксом отобранным файлам. Флаг повторяется по числу файлов. Состав
манифеста описан в [userland.md](userland.md#образ).

```bash
cargo xtask build-userland
cargo xtask build-userland --image user/testrunner/image.toml
cargo xtask build-userland --file /etc/motd=user/rootkeeper/motd.txt
```

## Тесты

Host-тесты:

```bash
cargo test --workspace
```

QEMU integration tests:

```bash
cargo xtask qemu-test [--timeout <сек>] [--spec <путь>]
```

По умолчанию тайм-аут — 60 секунд, спека — `devices/spec/qemu-aarch64-test.yaml`. `qemu-test` выполняет два прохода:

| Проход     | Feature ядра    | Userland-образ                | Что тестирует     |
|------------|-----------------|-------------------------------|-------------------|
| `kernel`   | `kernel-tests`  | `user/testrunner/image.toml`  | тесты внутри ядра |
| `userland` | —               | `user/testrunner/image.toml`  | тесты в userspace |

Ядро пересобирается между прогонами из-за различия feature-флагов. Образ у
проходов общий: под `kernel-tests` init-таском становится внутриядерный worker,
и userland в этом проходе не исполняется вовсе.
Выбор прохода для нового теста — в [`code-style.md`](code-style.md).

`--spec` меняет конфигурацию машины, оставляя проходы прежними. Спека по
умолчанию поднимает четыре CPU с GICv3; `devices/spec/qemu-aarch64-test-gicv2.yaml`
даёт ту же машину с GICv2, покрывая второй бэкенд контроллера прерываний:

```bash
cargo xtask qemu-test --spec devices/spec/qemu-aarch64-test-gicv2.yaml
```

`stress` спеку не выбирает и всегда идёт по умолчанию.

### Stress, race и fuzz

`stress` собирает отдельный образ testrunner, выбирает только
`#[kernel_test(stress)]` и повторяет одну последовательность операций для
диапазона seed. Диапазон проходит каждый stress-тест, и каждый отчитывается за
свой seed маркером `[STRESS-PASS]`.

```bash
cargo xtask stress --seed 100 --seeds 32 --steps 256 --timeout 900
```

При сбое выводятся seed, номер операции и её имя. Тем же seed и числом шагов
повторяется последовательность операций, но не их чередование: машина
многопроцессорная, и порядок, в котором CPU входят в ядро, seed не задаёт.
Поэтому слой проверяет границы жизненного цикла и ожиданий, а не конкретную
раскладку операций по времени.

Ядро этого прохода собирается с фичей `short-quantum`: квант вытеснения
становится 500 мкс вместо 10 мс, окно между операциями режется, и чередований
на тех же шагах получается заметно больше. Гейт `qemu-test` идёт с обычным
квантом.

`race` запускает под ThreadSanitizer host-тесты крейтов, чьё состояние делится
между CPU (`capability`, `collections`, `hal-aarch64-asid`, `memory`,
`scheduler`). Отбираются тесты с префиксом `race_`:

```bash
cargo xtask race
```

Fuzz-targets лежат в отдельном workspace `fuzz/` — он собирается своим
toolchain и не меняет target-конфигурацию основного bare-metal workspace:

```bash
cargo xtask fuzz fdt_parse_walk --seconds 60
cargo xtask fuzz ipc_wire_decode --seconds 60
cargo xtask fuzz ramfs_ops --seconds 60
cargo xtask fuzz userland_image_decode --seconds 60
```

Корпус находится в `fuzz/corpus/<target>/`, crash-артефакты — в
`fuzz/artifacts/<target>/` и не добавляются в Git автоматически. Для прямого
запуска доступны также `cargo fuzz build --fuzz-dir fuzz` и `cargo fuzz run
--fuzz-dir fuzz <target> -- <libFuzzer args>`.

Перед прогоном `xtask` пересобирает затравки корпуса в `fuzz/seeds/<target>/` и
передаёт директорию вторым корпусом, доступным только на чтение. Затравки
`userland_image_decode` собираются кодировщиком `userland-image` и обновляются
вместе с форматом образа.

## Проверка слоёв

```bash
cargo xtask check-layers
```

Сверяет рёбра зависимостей между крейтами основного и fuzz-workspace
(по `cargo metadata`) с правилами слоёв из [`architecture.md`](architecture.md).
При нарушениях печатает запрещённые рёбра и завершается с ошибкой.

## Проверка кодстайла

```bash
cargo xtask style [--all | --staged | --since <rev>]
                  [--write-baseline | --no-baseline]
                  [--format text|json] [--rule <id>]
```

Проверяет то, что не берут `cargo fmt` и `cargo clippy`: doc-комментарии, язык
строковых литералов, типографику прозы. Правила и уровни — в `style.toml`.

По умолчанию сверяются строки, изменённые относительно `origin/main`. `--all`
берёт дерево целиком, `--staged` — индекс, этот режим и стоит в
`hooks/pre-commit`.

Накопленные нарушения приняты снапшотом `style-baseline.txt` и молчат.
`--write-baseline` пересобирает его, `--no-baseline` печатает всё.

Запись снапшота, переставшую воспроизводиться, ищет только `--all`: правка,
убравшая накопленное нарушение, проходит `--since` и падает в CI кодом `2`.
Поэтому перед сдачей работы гоняется `--all`, а после него — `--write-baseline`,
если запись выгорела. Пересобранный снапшот обязан стать короче: выросший
означает, что в него попало нарушение нового кода, и чинится код, а не снапшот.

Под списком находок отчёт печатает пояснение каждого сработавшего правила:
норму, которую оно держит, и способ привести код к ней.

Единичное исключение — директива с обязательным обоснованием:

```rust
// style:allow(doc_on_declaration) regression guard kept by owner request
```

Директива без обоснования, с неизвестным правилом или не покрывшая находок сама
становится находкой.

Коды возврата: `1` — осталась находка уровня `deny`, `2` — отказ инструмента.
