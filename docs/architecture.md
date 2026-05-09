# Принципы архитектуры

## Многослойная архитектура

Крейты образуют DAG. Зависимости идут строго вниз:

```
crates/hal-aarch64 (точка входа)
    -> kernelspace, scheduler, kobject, syscall, userspace
    -> drivers-aarch64, memory, aarch64-paging
        -> drivers-common
            -> io, collections (утилиты, без бизнес-логики)
```

- Нижние слои не зависят от верхних.
- Платформенный код изолирован в `crates/hal-aarch64/` и `crates/drivers-aarch64/`.
- Переиспользуемая логика вынесена в отдельные крейты (`collections`, `io`, `util`, `fdt`).

## Трейты как абстракции

Ключевые подсистемы определены через трейты, а не конкретные типы:

- `Driver` / `DriverContext` — интерфейс драйвера устройства
- `FrameAllocator` — интерфейс аллокации физических фреймов
- `LockCell<T>` — стратегия синхронизации (замена `NoLockCell` -> `MutexCell`)
- `ByteSink` / `Writer` — вывод данных

Верхние слои зависят от трейтов, не от конкретных реализаций.

## Newtype-обёртки вместо примитивов

Запрещено использовать голые `usize` / `u64` для семантически различных сущностей:

- `PhysicalAddress(usize)` и `VirtualAddress(usize)` — невозможно перепутать
- `Frame` — номер фрейма, не адрес
- `Reg<T>` — типобезопасный дескриптор регистра (проверка ширины в compile-time)
- `Mmio` — обёртка базового адреса MMIO с volatile-доступом

Используйте `#[repr(transparent)]` для zero-cost обёрток.

## Гарантии на этапе компиляции

Переносите проверки из runtime в compile-time:

- Const generics для ёмкости: `Vec<T, const N: usize>`, `IntervalSet<T, const N: usize>`
- Const generics для выравнивания: `AlignedPhysicalAddress<const SHIFT: u8>`
- `PhantomData<T>` для привязки типа регистра к его ширине
- `const fn` конструкторы для использования в статиках

## Отсутствие глобального состояния

Глобальное состояние (`static mut`, lazy `static`) запрещено без явного обоснования. Вместо этого:

- Контексты: `DriverContext`, `ProbeContext` — передавайте зависимости явно через параметры
- Реестры: `DriverRegistry` — владеет состоянием, передаётся по ссылке

Исключения требуют комментария `// SAFETY:` с обоснованием (например, `STDOUT` в `klog`).

## Регистрация драйверов через секции линкера

Драйверы регистрируются через `register_driver!`, который размещает `DriverInfo` в секции `.drivers.early`:

```rust
register_driver!(
 UART_PL011_EARLY,
 probe = uart_pl011_probe
);
```

Это позволяет добавлять драйверы без модификации центрального реестра.

## Чистота функций

- Функции делают одну вещь и работают на одном уровне абстракции.
- Ранний возврат для guard-проверок: `if ... { return None; }`
- Цепочки итераторов предпочтительнее императивных циклов.
- `unwrap()` / `expect()` только в тестах и одноразовой инициализации; в остальных случаях `?`, `Option::map`, `if let`.

## Per-process AddressSpace

Каждому пользовательскому процессу принадлежит собственная нижняя половина
виртуального адресного пространства (`TTBR0_EL1` на AArch64); kernel-side
TTBR1 общий для всего ядра.

Ключевые точки:

- `scheduler::AddressSpace` — платформо-независимый wrapper:
  - `Kernel` — TTBR0=0, kernel-only;
  - `User(Arc<dyn MemoryMapper + Send + Sync>)` — собственный L0-root.
- `memory::memory_mapper::AddressSpaceFactory` — фабрика user-AS;
  реализуется в `hal-aarch64::memory::address_space_factory` поверх
  глобального `FrameAllocator`.
- `ArchContext::switch_address_space(Option<PhysicalAddress>)` — кросс-арх
  точка переключения root'а трансляции; на AArch64 пишет TTBR0_EL1 +
  делает `tlbi vmalle1`. В host-моке — счётчик-наблюдатель.
- `SchedulerInner::switch_to_next` сравнивает `process_id` prev/next;
  при изменении вызывает `switch_address_space` ДО `A::switch`.
- `SpawnConfig::address_space(SpawnAddressSpace)` управляет выбором AS
  для нового потока: `Kernel` (по умолчанию), `Inherit`, `NewUser`.

## Boot protocol

Граница между загрузчиком и ядром — `BootInfo` из `hal-common::boot` (`hw_description: HwDescription { Fdt | Acpi }`). `boot_main` принимает `&BootInfo` и не зависит от конкретного протокола.

Реализации протокола живут в `hal-aarch64::boot::protocol` под взаимоисключающими фичами:

- `boot-linux-arm64` (default) — Linux ARM64 Image: `.head` с magic `ARM\x64`, `_start` читает DTB-указатель из `x0`. Покрывает форматы `linux_arm64`, `android_boot_v1`, `android_boot_v2` (различаются только упаковкой).
- `boot-uefi` — зарезервирована, не реализована.

Конфликт фич ловится `compile_error!` в `protocol/mod.rs`. xtask и `build.rs` выбирают protocol-фичу и линкер-скрипт по `boot.format` из device spec.
