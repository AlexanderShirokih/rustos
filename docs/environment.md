# Окружение и зависимости

## Rust toolchain

Toolchain, компоненты и bare-таргет зафиксированы в
[rust-toolchain.toml](../rust-toolchain.toml).
`rustup` выбирает их при запуске Cargo из репозитория.

Системный таргет RustOS задан в `targets/`.
`core` и `std` для него собираются из исходников `rust-src`
([workspace.md](workspace.md#таргеты-сборки)).

## Host-зависимости

| Инструмент | Назначение |
|------------|------------|
| `cargo-binutils` | Извлечение бинарного образа через `cargo objcopy` |
| `qemu-system-aarch64` не ниже 8.1 | Запуск и тесты QEMU |
| C/C++ toolchain | Линковка host-программ, сборка зависимостей и fuzz-целей |
| `clang`, `lld` | Сборка библиотек C и C++ и C/C++-исходников программ userland |
| `meson`, `ninja-build`, `xz-utils` | Сборка библиотеки C из архива релиза |
| `cmake` | Сборка рантаймов C++ из архива релиза LLVM |
| `unzip` | Распаковка архивов стороннего кода |
| `mkbootimg` | Сборка Android boot image |
| `similarity-rs` | Проверка дублирования |
| `cargo-fuzz` | Сборка и запуск fuzz-целей |

Для QEMU-разработки на Debian bookworm `qemu-system-arm` ставится из bookworm-backports:

```bash
cargo install cargo-binutils
echo "deb http://deb.debian.org/debian bookworm-backports main" | sudo tee /etc/apt/sources.list.d/bookworm-backports.list
sudo apt-get update
sudo apt-get install -y gcc g++ libc6-dev meson ninja-build cmake unzip xz-utils
sudo apt-get install -y -t bookworm-backports qemu-system-arm
```

В `PATH` должны быть `clang` и компоновщик `ld.lld` той же версии.
Минимальную версию `clang` задаёт `MIN_CLANG_MAJOR` в
[libc_src.rs](../xtask/src/libc_src.rs).
Сборка использует архиватор `llvm-ar`, найденный через
`clang -print-prog-name=llvm-ar`, либо `ar`. Архиватор должен поддерживать
ELF-объекты. Для этой сборки подходит GNU `ar` из binutils. Системный `ar`
macOS не поддерживает ELF.

`mkbootimg` должен быть доступен в `PATH` при сборке Android-образа.
Нужную версию `similarity-rs` задаёт
[similarity.toml](../similarity.toml); команда
`cargo xtask similarity` печатает строку установки при отсутствии анализатора.

Окружение CI описано в [Dockerfile](../Dockerfile).

### macOS

Пакет `lld` предоставляет `ld.lld`, пакет `llvm` — `clang` и `llvm-ar`.
Каталог `bin` пакета `llvm` добавляется в `PATH` вручную:

```bash
brew install meson ninja cmake lld llvm
export PATH="$(brew --prefix llvm)/bin:$PATH"
```

Эта настройка выбирает `clang` из пакета `llvm`. `clang` от Apple также
подходит, если соответствует минимальной версии. Версию выбранного
компилятора показывает `clang -dumpversion`.

`mkbootimg` берётся из репозитория AOSP `platform/system/tools/mkbootimg`; каталог клона
добавляется в `PATH`.

## rust-analyzer

Для навигации по всему репозиторию rust-analyzer подключает workspace
профилей сборки через `linkedProjects`:

```json
{
  "rust-analyzer.linkedProjects": [
    "Cargo.toml",
    "tools/Cargo.toml",
    "kernel/platform/Cargo.toml",
    "lib/std-runtime/Cargo.toml",
    "user/Cargo.toml",
    "fuzz/Cargo.toml"
  ]
}
```

Манифесты и назначение профилей перечислены в
[workspace.md](workspace.md#профили-сборки).
