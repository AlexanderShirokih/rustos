# Команды

## Быстрая справка

| Действие                            | Команда                                                                                                              |
|-------------------------------------|----------------------------------------------------------------------------------------------------------------------|
| Основные проверки одной командой    | `cargo xtask check [--all] [--only <name>] [--list] [--fail-fast]`                                                    |
| Host-тесты                          | `cargo test --workspace`                                                                                             |
| Host-clippy                         | `cargo clippy --workspace`                                                                                           |
| AArch64 clippy                      | `cargo clippy --workspace --exclude xtask --exclude userland-build --exclude ipc-test --exclude style-tool --exclude userland-image-tool --target aarch64-unknown-none` |
| Cargo в workspace userspace         | `cargo xtask userland <аргументы cargo>`                                                                             |
| Форматирование                      | `cargo xtask fmt [--check]`                                                                                          |
| Ссылки в документации               | `RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D warnings" cargo doc --no-deps --workspace`                       |
| Исходники `std` с оверлеем          | `cargo xtask std-src [--force] [--verify]`                                                                           |
| Проверка слоёв                      | `cargo xtask check-layers`                                                                                           |
| Проверка кодстайла                  | `cargo xtask style [--all] [--staged] [--since <rev>] [--base <rev>]`                                                |
| Поиск дублирующегося кода           | `cargo xtask similarity [--write-baseline] [--no-baseline] [--base <rev>]`                                           |
| Сборка userland                     | `cargo xtask build-userland [--image <путь>] [--file <путь>=<источник>]`                                             |
| Загрузочный образ (userland + ядро) | `cargo xtask build devices/spec/<device>.yaml [--image <путь>] [--file <путь>=<источник>]`                           |
| Запуск после сборки                 | `cargo xtask build devices/spec/<device>.yaml --run`                                                                 |
| QEMU integration tests              | `cargo xtask qemu-test [--timeout <сек>] [--spec <путь>]`                                                            |
| Userspace stress в QEMU             | `cargo xtask stress [--seed <n>] [--seeds <n>] [--steps <n>] [--epoch <n>] [--shard <n>] [--shards <n>] [--timeout <сек>]` |
| Замеры файловых плоскостей в QEMU   | `cargo xtask bench [--timeout <сек>]`                                                                                |
| Host race под ThreadSanitizer       | `cargo xtask race`                                                                                                    |
| Ограниченный fuzz-прогон            | `cargo xtask fuzz <target> [--seconds <сек>]`                                                                         |

Отделённые workspace `--workspace` не видит: форматирование покрывает
`cargo xtask fmt`, clippy идёт своим прогоном на каждый
([«Отделённые workspace»](#отделённые-workspace)).

Отдельные команды таблицы нужны, когда проверяется одна вещь. Дифф целиком
проходит через [`cargo xtask check`](#основные-проверки).

## Основные проверки

```
cargo xtask check [--all] [--only <name>] [--list] [--fail-fast]
```

Прогоняет по рабочему дереву весь основной инструментарий: форматирование,
линтеры, тесты, проверки слоёв, стиля и документации. Отдельные команды таблицы
остаются для случая, когда проверяется одна вещь. По умолчанию идут проверки,
которым хватает обычной сборки дерева.

Набор проверок и их команды задаёт `checks.toml` в корне дерева.

| Флаг            | Назначение                                                                                          |
|-----------------|------------------------------------------------------------------------------------------------------|
| `--all`         | Добавить проверки, требующие инструмента вне обычной сборки                                           |
| `--only <name>` | Запустить только названную проверку; повторяется по числу проверок                                    |
| `--list`        | Напечатать состав отобранного набора, ничего не запуская                                              |
| `--fail-fast`   | Остановиться на первой неудаче; по умолчанию прогон идёт до конца и сводит упавшие проверки в отчёт    |

```bash
cargo xtask check --only style --only clippy-host
```

## Сборка

```
cargo xtask build <spec> [--run] [--debug] [--features <features>] [--image <путь>] [--file <путь>=<источник>]
```

Читает YAML-спеку устройства, собирает `target/build/userland.img`, затем
собирает `hal-aarch64` под `aarch64-unknown-none --release` и упаковывает
результат по правилам `boot.format`. Программы образа идут под свой таргет, и
стандартная библиотека поднимается им из исходников, которые готовит
`cargo xtask std-src` ([workspace.md](workspace.md#таргеты-сборки)).

| Флаг                       | Назначение                                                                                                                           |
|----------------------------|---------------------------------------------------------------------------------------------------------------------------------------|
| `--run`                    | После сборки выполнить команды из секции `run` YAML-спеки                                                                             |
| `--debug`                  | После сборки выполнить команды из секции `debug` YAML-спеки                                                                           |
| `--features <список>`      | Дополнительные features (через запятую)                                                                                              |
| `--image <путь>`           | Относительный путь к TOML-манифесту userland-образа. По умолчанию `user/rootkeeper/image.toml`                                        |
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

Собирает только `target/build/userland.img`. Флаги те же, что у `build`. Состав
манифеста описан в [userland.md](userland.md#образ).

```bash
cargo xtask build-userland --image user/testrunner/image.toml
cargo xtask build-userland --file /boot/rootkeeper/motd=user/rootkeeper/motd.txt
```

## Тесты

Уровни тестов и выбор прохода для нового теста — в [testing.md](testing.md).

```bash
cargo test --workspace
cargo xtask qemu-test [--timeout <сек>] [--spec <путь>]
```

По умолчанию тайм-аут — 60 секунд, спека — `devices/spec/qemu-aarch64-test.yaml`
с четырьмя CPU и GICv3. `--spec` меняет конфигурацию машины, оставляя проходы
прежними; `devices/spec/qemu-aarch64-test-gicv2.yaml` даёт ту же машину с GICv2,
покрывая второй бэкенд контроллера прерываний:

```bash
cargo xtask qemu-test --spec devices/spec/qemu-aarch64-test-gicv2.yaml
```

### Замеры

`bench` собирает образ testrunner с профилем `bench` и выбирает только
`#[kernel_test(bench)]`. Каждый замер печатает строку `[TEST-REPORT]` с видом
плоскости, размером операции и наносекундами на операцию.

```bash
cargo xtask bench
```

Прогон идёт под TCG, где нет модели кеша и TLB. Зависимость цены операции от
размера региона наблюдаема и переносима, абсолютная пропускная способность и
соотношение цены IPC к цене копирования — нет.

### Stress, race и fuzz

`stress` собирает отдельный образ testrunner, выбирает только
`#[kernel_test(stress)]` и повторяет одну последовательность операций для
диапазона seed; каждый тест отчитывается за свой seed маркером `[STRESS-PASS]`.
Шаги одного seed разбиты на эпохи длиной `--epoch`, и каждая отчитывается
маркером `[STRESS-EPOCH]` со снятой на её границе величиной бюджета. Спеку
машины команда не выбирает и всегда идёт по умолчанию.

```bash
cargo xtask stress --seed 100 --seeds 32 --steps 256 --epoch 64 --timeout 900
```

`--shards` делит диапазон seed на равные части, `--shard` называет часть,
которую берёт прогон. Диапазон с остатком и номер за пределами числа шардов
команда отвергает до сборки образа.

```bash
cargo xtask stress --seeds 64 --steps 16384 --epoch 256 --shard 1 --shards 4
```

Тем же seed повторяется последовательность операций, но не их чередование:
машина многопроцессорная, и порядок, в котором CPU входят в ядро, seed не
задаёт. Поэтому слой проверяет границы жизненного цикла и ожиданий, а не
конкретную раскладку операций по времени. Ядро этого прохода собирается с фичей
`short-quantum`: квант вытеснения становится 500 мкс вместо 10 мс, и
чередований на тех же шагах получается заметно больше.

`race` запускает под ThreadSanitizer host-тесты крейтов, чьё состояние делится
между CPU (`capability`, `collections`, `hal-aarch64-asid`, `memory`,
`scheduler`). Отбираются тесты с префиксом `race_`.

```bash
cargo xtask race
```

Fuzz-targets лежат в отделённом workspace `fuzz/`:

```bash
cargo xtask fuzz fdt_parse_walk --seconds 60
cargo xtask fuzz ipc_wire_decode --seconds 60
cargo xtask fuzz ramfs_ops --seconds 60
cargo xtask fuzz userland_image_decode --seconds 60
```

Корпус находится в `fuzz/corpus/<target>/`, crash-артефакты — в
`fuzz/artifacts/<target>/` и не добавляются в Git автоматически. Перед прогоном
`xtask` пересобирает затравки корпуса в `fuzz/seeds/<target>/` и передаёт
директорию вторым корпусом, доступным только на чтение. Для прямого запуска
доступны `cargo fuzz build --fuzz-dir fuzz` и `cargo fuzz run --fuzz-dir fuzz
<target> -- <libFuzzer args>`.

## Исходники `std` с оверлеем

```bash
cargo xtask std-src [--force] [--verify]
```

Копирует дерево `library` из компонента `rust-src` зафиксированного toolchain в
`target/std-src` и накладывает оверлей `patches/std-molecular.patch` — он
добавляет платформенный слой RustOS. Установленный toolchain не трогается:
sysroot общий, копия принадлежит сборке. Повторный вызов молчит, пока не
сменились ни оверлей, ни sysroot; `--force` пересоздаёт дерево всегда.

Вторая половина подготовки — крейты дерева, которые собираются внутрь `std`
(объявляют фичу `rustc-dep-of-std`). Юниты графа `-Zbuild-std` cargo
пересобирает по смене зависимостей и флагов, но не по смене исходников: правка в
`lib/runtime` иначе молча уходит в собранный `std` старой версией. По отпечатку
их файлов эти пакеты чистятся `cargo clean`.

`--verify` дополнительно собирает `hello-std`. Этим ловится расхождение оверлея
с пином из `rust-toolchain.toml`, из-за которого `git apply` перестаёт ложиться,
и поломка дерева, которая ложится, но не собирается. Проверка стоит дороже
остальных — она поднимает `core`, `alloc` и саму стандартную библиотеку из
исходников, — поэтому в CI она идёт по собственному условию и запускается по
изменению того, из чего собрана.

Команда печатает путь к дереву `library`; сборкой по нему распоряжается
`cargo xtask userland`.

## Отделённые workspace

Состав и назначение — в [workspace.md](workspace.md#отделённые-workspace). Cargo
над ними запускается по manifest-пути:

```bash
cargo clippy --manifest-path lib/runtime/Cargo.toml \
             --target targets/aarch64-unknown-molecular.json \
             -Zbuild-std=core,alloc -Zjson-target-spec -- -D warnings
```

Крейты, собираемые внутрь `std`, линтуются под тем же таргетом, под который их
собирает она: готового `core` для него нет, поэтому `core` и `alloc` идут из
исходников.

Таргет и флаги сборки `std` workspace `user/` держит в `user/.cargo/config.toml`,
а путь к дереву исходников приходит переменной окружения. И то и другое ставит
`cargo xtask userland`: он материализует дерево и запускает cargo из каталога
workspace.

```bash
cargo xtask userland build --release --workspace
cargo xtask userland clippy --workspace --all-targets -- -D warnings
cargo xtask userland doc --no-deps --workspace
```

## Форматирование

```bash
cargo xtask fmt [--check]
```

Прогоняет rustfmt по корневому workspace и по каждому отделённому. `--check`
печатает файлы, которые расходятся с `rustfmt.toml`, и завершается с ошибкой,
ничего не переписав. Этот режим стоит в `hooks/pre-commit` и в CI.

Голый `cargo fmt --all` видит только членов своего workspace, и отделённый до
гейта не доходит.

## Проверка слоёв

```bash
cargo xtask check-layers
```

Сверяет рёбра зависимостей между крейтами корневого workspace и всех отделённых
(по `cargo metadata`) с правилами слоёв из [workspace.md](workspace.md#домены).
При нарушениях печатает запрещённые рёбра и завершается с ошибкой.

Заодно она сверяет таблицу `[workspace.lints]` каждого отделённого workspace с
корневой: наследовать её вне корневого дерева нечем, а разойдясь, крейт
собирается по более слабым правилам.

Каждая объявленная path-зависимость сверяется с исходниками пакета: манифест
задаёт граф, по которому идёт проверка слоёв, и ребро без единой ссылки в коде
этот граф искажает.

## Ссылки в документации

```bash
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D warnings" cargo doc --no-deps --workspace
```

Ссылка на переименованный или приватный элемент молча пропадает из документации,
оставляя текст, который читателю некуда открыть. Флаг переводит такую ссылку в
ошибку вместе с прочими предупреждениями rustdoc.

## Проверка кодстайла

```bash
cargo xtask style [--all | --staged | --since <rev>]
                  [--write-baseline | --no-baseline] [--base <rev>]
                  [--format text|json] [--rule <id>]
```

Проверяет то, что не берут `cargo fmt` и `cargo clippy`: doc-комментарии, язык
строковых литералов, типографику прозы. Правила и уровни — в `style.toml`, а
норму каждого сработавшего правила отчёт печатает под списком находок.

По умолчанию сверяются строки, изменённые относительно `origin/main`. `--all`
берёт дерево целиком, `--staged` — индекс, этот режим и стоит в
`hooks/pre-commit`.

Накопленные нарушения приняты снапшотом `style-baseline.txt` и молчат.
`--write-baseline` пересобирает его, `--no-baseline` печатает всё. Запись
снапшота, переставшую воспроизводиться, ищет только `--all`, поэтому перед
сдачей работы гоняется `--all`, а после него — `--write-baseline`, если запись
выгорела. Пересобранный снапшот обязан стать короче: выросший означает, что в
него попало нарушение нового кода, и чинится код, а не снапшот.

`--base <rev>` роняет прогон на росте числа принятых нарушений у правила: этим
режимом снапшот проверяется в CI. Ревизию без записи снапшота сверять не с чем.

Единичное исключение — директива с обязательным обоснованием:

```rust
// style:allow(doc_on_declaration) regression guard kept by owner request
```

Директива без обоснования, с неизвестным правилом или не покрывшая находок сама
становится находкой.

Коды возврата: `1` — осталась находка уровня `deny`, `2` — отказ инструмента.

## Поиск дублирующегося кода

```bash
cargo xtask similarity [--write-baseline | --no-baseline] [--base <rev>]
```

Сообщает пары функций одной формы. Версия анализатора `similarity-rs`,
разбираемые пути и пороги заданы в `similarity.toml`. Отсутствие анализатора и
расхождение версии команда называет вместе с командой установки.

Принятые пары держит `similarity-baseline.txt` строкой на пару в виде
`<путь>|<символ>|<путь>|<символ>`. Пара вне записи роняет прогон.
`--write-baseline` пересобирает запись, `--no-baseline` печатает и принятые
пары, `--base <rev>` роняет прогон на росте числа принятых пар у файла: этим
режимом запись проверяется в CI.

## Хук pre-commit

`hooks/pre-commit` гоняет перед коммитом обе проверки исходников:
[`cargo xtask fmt --check`](#форматирование) и `cargo xtask style --staged`. Git
берёт хуки из этой директории после

```bash
git config core.hooksPath hooks
```
