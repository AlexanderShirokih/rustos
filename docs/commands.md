# Команды

Команды запускаются из корня репозитория через `cargo xtask`.
Актуальные аргументы каждой команды доступны через `--help`.

## Быстрая справка

| Действие | Команда |
|----------|---------|
| Основные проверки | `cargo xtask check` |
| Host-тесты | `cargo xtask test` |
| Clippy | `cargo xtask clippy` |
| Rustdoc | `cargo xtask doc` |
| Форматирование | `cargo xtask fmt` |
| Проверка зависимостей | `cargo xtask check-layers` |
| Проверка стиля | `cargo xtask style` |
| Поиск дублирования | `cargo xtask similarity` |
| Загрузочный образ | `cargo xtask build <spec>` |
| Образ userland | `cargo xtask build-userland` |
| Cargo в userland | `cargo xtask userland <аргументы cargo>` |
| Исходники стандартной библиотеки | `cargo xtask std-src` |
| Библиотека C | `cargo xtask libc-src` |
| Сторонний код | `cargo xtask third-party` |
| Тесты под QEMU | `cargo xtask qemu-test` |
| Замеры под QEMU | `cargo xtask bench` |
| Стресс-тесты под QEMU | `cargo xtask stress` |
| ThreadSanitizer | `cargo xtask race` |
| Ограниченный fuzz-прогон | `cargo xtask fuzz <target>` |

Команды `fmt`, `clippy` и `doc` обходят workspace по
[профилям сборки](workspace.md#профили-сборки). Прямой
`cargo ... --workspace` обрабатывает только выбранный workspace.
Команда `test` запускает host-тесты профилей `common` и `host`.

## Основные проверки

```text
cargo xtask check [--all] [--only <name>] [--list] [--fail-fast]
```

Состав и команды проверок задаёт [checks.toml](../checks.toml).
По умолчанию запускается основной набор.

| Флаг | Назначение |
|------|------------|
| `--all` | Добавить проверки, требующие дополнительных инструментов |
| `--only <name>` | Выбрать проверку; флаг можно повторить |
| `--list` | Показать выбранный набор без запуска |
| `--fail-fast` | После отказа не начинать новые проверки |

Проверки одной `lane` выполняются последовательно; разные очереди
могут выполняться параллельно.

```bash
cargo xtask check --list
cargo xtask check --only style --only clippy-host
```

## Сборка

```text
cargo xtask build <spec> [--run] [--debug] [--features <features>]
                       [--image <path>] [--file <path>=<source>]
```

Команда читает YAML-конфигурацию устройства, собирает userland и ядро,
затем создаёт загрузочный образ по `boot.format`.

| Флаг | Назначение |
|------|------------|
| `--run` | Выполнить команды `run` конфигурации после сборки |
| `--debug` | Выполнить команды `debug` после сборки |
| `--features` | Добавить features ядра через запятую |
| `--image` | Выбрать манифест образа userland |
| `--file` | Добавить файл или набор файлов; флаг можно повторить |

По умолчанию используется
[user/system/rootkeeper/image.toml](../user/system/rootkeeper/image.toml).
Пути источников задаются относительно корня репозитория.

```bash
cargo xtask build devices/spec/qemu-aarch64.yaml --run
```

| Формат | Артефакты |
|--------|-----------|
| Общий для всех | `target/build/userland.img` |
| `linux_arm64` | `target/build/kernel.bin`; userland передаётся как initrd |
| `android_boot_v1` | `target/build/boot.img`; userland в ramdisk, DTB после kernel.gz |
| `android_boot_v2` | `target/build/boot.img`; userland в ramdisk, DTB отдельным полем |
| `uefi` | Сборка не реализована |

## Userland

```text
cargo xtask build-userland [--image <path>] [--arch <name>]
                          [--file <path>=<source>]
```

Команда собирает `target/build/userland.img`.
`--image` и `--file` имеют тот же смысл, что у `build`.
`--arch` выбирает архитектуру реестра; по умолчанию берётся первая.
Формат манифеста описан в [userland.md](userland.md#образ).

```bash
cargo xtask build-userland --image user/testing/testrunner/image.toml
```

Для прямых команд Cargo в workspace `user/` используется:

```bash
cargo xtask userland build --release --workspace
cargo xtask userland clippy --workspace --all-targets -- -D warnings
cargo xtask userland doc --no-deps --workspace
```

`xtask` подготавливает исходники `std` и устанавливает окружение сборки.

## Тесты

Размещение и уровни тестов описаны в [testing.md](testing.md).
`cargo xtask test` запускает host-тесты профилей `common` и `host`.

```text
cargo xtask qemu-test [--timeout <seconds>] [--spec <path>] [--durations <n>] [<filter>]
```

Тайм-аут по умолчанию — 120 секунд, конфигурация —
[devices/spec/qemu-aarch64-test.yaml](../devices/spec/qemu-aarch64-test.yaml).
`--durations <n>` выводит после каждого прохода `n` самых долгих тестов.
Длительность измеряется на хосте между маркерами начала и результата теста.

`<filter>` запускает только тесты, имя которых содержит указанную подстроку.
Фильтр без совпадений завершает команду отказом:

```bash
cargo xtask qemu-test badge_departs
```

Альтернативный контроллер прерываний проверяется так:

```bash
cargo xtask qemu-test --spec devices/spec/qemu-aarch64-test-gicv2.yaml
```

После изменения диагностируемого контракта или версии компилятора
ожидаемую диагностику `trybuild` можно обновить командой:

```bash
TRYBUILD=overwrite cargo test -p syscall --test compile_fail
```

Изменения `.stderr` проверяются по ожидаемому отказу компиляции.

### Замеры

`cargo xtask bench [<filter>]` запускает файловые замеры testrunner.
`<filter>` отбирает замеры по подстроке имени, как у `qemu-test`.
`--timeout` задаёт срок прогона в секундах; по умолчанию 120.
Результаты публикуются строками `[TEST-REPORT]`.

Прогон под QEMU TCG измеряет поведение этой конфигурации.
Абсолютные времена и пропускная способность не подтверждают скорость
на устройстве.

### Stress, race и fuzz

```text
cargo xtask stress [--seed <n>] [--seeds <n>] [--steps <n>]
                   [--epoch <n>] [--shard <n>] [--shards <n>]
                   [--cpus <n>] [--memory <MiB>] [--timeout <seconds>]
```

Стресс-прогон повторяет операции для диапазона seed.
`--steps` задаёт число операций, `--epoch` — длину эпохи.
`--shards` делит диапазон на равные части, `--shard` выбирает часть
с нулевой нумерацией. Неделимый диапазон и неверный номер отклоняются.
`--cpus` и `--memory` задают размер машины QEMU; `--cpus` задаёт и число
потоков параллельных стресс-тестов.

Seed повторяет последовательность операций; чередование потоков
зависит от планирования.

```bash
cargo xtask stress --seed 100 --seeds 32 --steps 256 --epoch 64 --timeout 900
cargo xtask race
cargo xtask fuzz ipc_wire_decode --seconds 60
```

`race` запускает host-тесты с именами `race_*` под ThreadSanitizer.
Имена должны быть уникальны; найденный тест обязан отчитаться о прохождении.

Fuzz-цели находятся в [fuzz/fuzz_targets](../fuzz/fuzz_targets).
Их список показывает `cargo fuzz list --fuzz-dir fuzz`.
`--seconds` ограничивает время прогона; по умолчанию 60 секунд.
Корпусы хранятся в `fuzz/corpus/<target>/`, найденные ошибки —
в `fuzz/artifacts/<target>/`. Эти файлы автоматически в Git не добавляются.

## Исходники стандартной библиотеки

```text
cargo xtask std-src [--force] [--verify [--arch <name>]]
```

Команда копирует `library` из `rust-src` в `target/std-src` и
применяет [patches/std-molecular.patch](../patches/std-molecular.patch).
Копия обновляется при изменении toolchain или оверлея.
`--force` пересоздаёт её.

`--verify` собирает `hello-std` для зарегистрированных архитектур.
`--arch` ограничивает эту проверку одной архитектурой.
Успех подтверждает применимость оверлея и сборку программы; исполнение
проверяется отдельно.

## Исходники библиотеки C

```text
cargo xtask libc-src [--force]
```

Команда загружает закреплённые релизы picolibc и рантаймов C++ LLVM,
проверяет контрольные суммы и размещает исходники в `target/libc-src` и
`target/llvm-src`. Для каждой зарегистрированной архитектуры она собирает
библиотеку C, libc++, libc++abi и libunwind в `target/libc-sysroot/<arch>`.

Повторный вызов использует готовые библиотеки, если параметры сборки
совпадают. Команда пересобирает их при смене релизов, инструментов,
архиватора или флагов; `--force` требует пересборки.
Параметры и состав библиотек определены в
[libc_src.rs](../xtask/src/libc_src.rs).

## Сторонний код

```text
cargo xtask third-party [--force]
```

Команда загружает версии стороннего кода, закреплённые в реестре
[third_party.rs](../xtask/src/third_party.rs), и размещает их в
`target/third-party/<имя>`.
`--force` заново подготавливает каждый каталог.

Сборка userland автоматически подготавливает записи с исходниками.
Записи с данными подготавливаются, когда манифест образа или `--file`
ссылается на файл в их каталоге.

## Clippy и rustdoc

```text
cargo xtask clippy [--host] [--target] [--arch <name>]
cargo xtask doc
```

Без фильтров clippy проверяет все профили. `--host` выбирает host-сторону,
`--target` — целевые архитектуры, повторяемый `--arch` — названные
архитектуры.

`doc` собирает rustdoc по профилям с предупреждениями как ошибками,
включая некорректные ссылки на элементы Rust.

## Форматирование

`cargo xtask fmt` форматирует Rust-код всех workspace.
`--check` сообщает расхождения и завершает команду с ошибкой.
Настройки заданы в [rustfmt.toml](../rustfmt.toml).

## Проверка слоёв

`cargo xtask check-layers` проверяет зависимости workspace по
[правилам доменов](workspace.md#домены), архитектур и владельцев состояния.
Объявленные path-зависимости также сверяются с использованием в исходниках.

## Проверка кодстайла

```text
cargo xtask style [--all | --staged | --since <rev>]
                  [--write-baseline | --no-baseline] [--base <rev>]
                  [--format text|json] [--rule <id>]
```

Правила заданы в [style.toml](../style.toml).
По умолчанию проверяются строки, изменённые относительно `origin/main`.

| Флаг | Назначение |
|------|------------|
| `--all` | Проверить всё дерево |
| `--staged` | Проверить содержимое индекса |
| `--since` | Выбрать базу сравнения изменённых строк |
| `--rule` | Ограничить набор правил |
| `--format` | Выбрать текстовый или JSON-отчёт |
| `--no-baseline` | Показать принятые нарушения |
| `--write-baseline` | Пересоздать baseline |
| `--base` | Запретить рост принятых нарушений относительно ревизии |

Принятые нарушения хранятся в [style-baseline.txt](../style-baseline.txt).
Новые нарушения исправляются в тексте и коде. Рост baseline допускается
для добавленного правила; прочие записи должны сокращаться.

Точечное исключение задаётся директивой с обоснованием.
Неизвестное правило, пустое обоснование или неиспользованная директива
также считаются нарушениями.

Коды выхода: `1` — нарушения уровня `deny`, `2` — отказ инструмента.

## Поиск дублирующегося кода

```text
cargo xtask similarity [--write-baseline | --no-baseline] [--base <rev>]
```

Версия анализатора, пути и пороги заданы в
[similarity.toml](../similarity.toml). Команда проверяет сходные функции
по [similarity-baseline.txt](../similarity-baseline.txt).
Отсутствующий или несовместимый анализатор приводит к отказу с командой
установки.

`--write-baseline` пересоздаёт принятые пары,
`--no-baseline` показывает их вместе с новыми,
`--base` запрещает рост числа принятых пар по файлу.

## Хук pre-commit

[hooks/pre-commit](../hooks/pre-commit) проверяет форматирование при изменении
Rust-файлов и стиль при изменении Rust-файлов или конфигурации стиля.
Для одной правки Markdown проверка стиля запускается отдельно.

Хук подключается командой:

```bash
git config core.hooksPath hooks
```
