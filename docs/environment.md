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
| `qemu-system-aarch64` | Запуск и тесты QEMU |
| C/C++ toolchain | Линковка host-программ, сборка зависимостей и fuzz-целей |
| `clang`, `lld` | Сборка библиотек C и C++ и C/C++-исходников программ userland |
| `meson`, `ninja-build`, `xz-utils` | Сборка библиотеки C из архива релиза |
| `cmake` | Сборка рантаймов C++ из архива релиза LLVM |
| `unzip` | Распаковка архивов стороннего кода |
| `mkbootimg` | Сборка Android boot image |
| `similarity-rs` | Проверка дублирования |
| `cargo-fuzz` | Сборка и запуск fuzz-целей |

Для QEMU-разработки на Debian:

```bash
cargo install cargo-binutils
sudo apt-get install -y gcc g++ libc6-dev qemu-system-arm meson ninja-build cmake unzip xz-utils
```

`clang` должен быть доступен в `PATH` под именем `clang`, а компоновщик `lld` той же
версии — под именем `ld.lld`. Минимальную версию `clang` задаёт `MIN_CLANG_MAJOR` в
[libc_src.rs](../xtask/src/libc_src.rs).
Архиватором библиотек C и C++ и исходников программ служит `llvm-ar`, если его
находит `clang` (`clang -print-prog-name=llvm-ar`), иначе `ar`. Архиватор должен
понимать ELF-объекты: GNU `ar` из binutils подходит, `ar` macOS — нет.

`mkbootimg` должен быть доступен в `PATH` при сборке Android-образа.
Нужную версию `similarity-rs` задаёт
[similarity.toml](../similarity.toml); команда
`cargo xtask similarity` печатает строку установки при отсутствии анализатора.

Окружение CI описано в [Dockerfile](../Dockerfile).

### macOS

Компоновщик `ld.lld` даёт пакет `lld`, архиватор `llvm-ar` — пакет `llvm`. Пакет `llvm` не
ставит программы в `PATH`, его каталог `bin` добавляется вручную:

```bash
brew install meson ninja cmake lld llvm
export PATH="$PATH:$(brew --prefix llvm)/bin"
```

Каталог `bin` в конце `PATH` оставляет `clang` от Apple, в начале — подставляет `clang` из
`llvm`. Библиотека C собирается с любым из них.

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
