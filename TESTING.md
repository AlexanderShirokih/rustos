# Тесты проекта

Ниже короткая памятка по запуску ключевых наборов тестов. Предполагается установленный `rustup` со стабильным toolchain и базовыми инструментами (`cargo`, `rustc`).

## Базовые зависимости
- Обновите инструменты: `rustup update`
- Установите дополнительные таргеты:
  - `rustup target add aarch64-apple-darwin` — для запуска тестов архитектурного слоя на хосте Apple Silicon.
  - (опционально) `rustup target add aarch64-unknown-none` — если потребуется собирать ядро под «голое» железо.

## Быстрые проверки
- `cargo test --target aarch64-apple-darwin` — прогон всех доступных тестов по рабочей платформе.
- `cargo test -p memory --target aarch64-apple-darwin` — модульные и property-based тесты менеджера физической памяти.  
  Property-тесты (`memory/tests/property.rs`) включены по умолчанию. Для запуска только их:
  ```bash
  cargo test -p memory --target aarch64-apple-darwin --test property -- --nocapture
  ```
- `cargo test -p util --target aarch64-apple-darwin` — проверки вспомогательного крейта (на данный момент тестов нет, команда завершится сразу).

## Архитектурный слой (`arch/aarch64`)
Тесты виртуальной памяти находятся в `arch/aarch64/tests/virtual_mem.rs`.

1. Убедитесь, что добавлен таргет:  
   `rustup target add aarch64-apple-darwin`
2. Запустите:  
   ```bash
   cargo test -p arch-aarch64 --target aarch64-apple-darwin -- --nocapture
   ```
   Бинарь `kernel-aarch64` по-прежнему предназначен для таргета `aarch64-unknown-none`, поэтому для его сборки используйте соответствующий таргет и `cargo -Zbuild-std`. На macOS тестовый прогон работает из коробки: хостовая сборка использует заглушку `main`, а секции `.bss.heap/.bss.stack` отключаются автоматически.

### Известные ограничения
- Крейт `arch-aarch64` по умолчанию `#![no_std]`. Если нужна именно сборка ядра под `aarch64-unknown-none`, используйте nightly и `cargo -Zbuild-std=core,alloc`.

## Полезные приёмы
- Прогнать конкретный тест:  
  `cargo test -p arch-aarch64 map_and_translate_single_page -- --nocapture`
  
Дополняйте файл по мере появления отдельных сценариев (например, запуск интеграций в симуляторе или стресс-тестов с feature-флагами).

