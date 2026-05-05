# Тестирование

## Интеграционные тесты (основной подход)

Расположены в директории `tests/` каждого крейта:

- Общие хелперы в `tests/common/mod.rs` (импорт через `mod common;`).
- Хелперы создают тестовые объекты: `single_region_allocator()`, `make_range()`, `frame_to_address()`.
- Один файл тестов на модуль: `tests/frame_allocator.rs`, `tests/frame_bitmap.rs`.

## Юнит-тесты

Для изолированной логики — `#[cfg(test)] mod tests` в конце файла.

## Именование тестов

Описательное, по паттерну `действие_условие_результат`:

- `new_with_multiple_regions_succeeds`
- `allocate_switches_to_next_region_when_exhausted`
- `deallocate_same_frame_twice_returns_not_allocated`
- `reserve_spanning_region_boundary_fails`

## Группировка

Тесты группируются в логические секции с комментариями-заголовками.

## Паттерны

- `assert!` / `assert_eq!` с описательным сообщением последним аргументом.
- `matches!(err, FrameError::NotAllocated)` для проверки вариантов enum.
- `#[should_panic(expected = "...")]` для тестов паник.
- `unwrap()` / `unwrap_err()` допустимы в тестах.

## Запуск тестов

```bash
# Все host-совместимые крейты
cargo test --workspace --exclude drivers-aarch64 --exclude hal-aarch64 --exclude main
```

> Крейты `drivers-aarch64`, `hal-aarch64` и `main` содержат aarch64 inline assembly и не компилируются на x86_64. Всегда исключайте их при запуске на host.

## QEMU integration tests

Используется production boot path: MMU, GIC, scheduler, аллокатор —
всё инициализируется штатно, init-таск вместо демо-процессов прогоняет
кейсы и завершает QEMU через semihosting. Подходит для тестов
аллокатора, многозадачности, driver-init, kobject-сценариев.

Кейсы регистрируются `register_test!` в `kernel/src/qemu_tests.rs`
(или другом модуле под `cfg(feature = "qemu-tests")`), вызывая живые
сервисы ядра.

```bash
cargo xtask build devices/spec/qemu-aarch64-test.yaml \
    --features qemu-tests --run
```

Контракт маркеров: `[TEST-RUN: N]`, `[TEST-START: name]`,
`[TEST-PASS: name]`, `[TEST-DONE: N]` — успех; `[TEST-FAIL: <reason>]
at <file>:<line>` — провал. QEMU выходит через ARM semihosting с
реальным exit-кодом; ненулевой код пробрасывается как провал теста.
