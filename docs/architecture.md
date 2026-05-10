# Принципы архитектуры

## Слои и зависимости

Крейты образуют DAG. Нижние подсистемы не зависят от платформенного
слоя и orchestration-кода.

- `hal-aarch64` — boot, исключения, MMU setup, platform context switch.
- `kernelspace` — сборка ядра из сервисов: драйверы, scheduler, syscall bridge, user init.
- `scheduler`, `kobject`, `syscall`, `userspace`, `memory` — архитектурно-независимые механизмы ядра.
- `drivers-aarch64` — реализации устройств; общие интерфейсы живут в `drivers-common`.
- `collections`, `io`, `fdt`, `klog`, `util` — базовые no_std-библиотеки.

Платформенная конкретика выносится за trait-интерфейсы. Общий код не
должен импортировать `hal-aarch64` и не должен знать детали конкретного
MMU, interrupt controller или boot-протокола.

## Типы вместо примитивов

Для семантически разных значений используются newtype-обёртки:

- `PhysicalAddress`, `VirtualAddress`, page-aligned варианты адресов.
- `Frame`, `ProcessId`, `ThreadId`, `HandleId`, `Koid`.
- `Priority`, `UserBootstrapArg`, `AddressSpaceHandle`.
- `Reg<T>`/platform-типы регистров в платформенном слое.

Новые публичные API не должны принимать голые `usize`/`u64`, если у
значения есть собственная предметная семантика.

## Compile-time гарантии

- Const generics для ёмкости и выравнивания там, где это часть типа.
- `NonZero*` для ABI/структур, где ноль невозможен (`HandleId`, размеры VM-регионов).
- `#[repr(transparent)]` для zero-cost wrappers.
- Закрытые enum'ы для исчерпывающих `match` по KO, syscall op и состояниям.

## Address space

`scheduler::AddressSpace` имеет два варианта:

- `Kernel` — общее kernel-only пространство для kernel-thread'ов.
- `User(Arc<dyn MemoryMapper + Send + Sync>)` — отдельный mapper процесса.

`AddressSpaceFactory` создаёт user-AS, platform layer реализует
активацию через `ArchContext::switch_address_space`. Scheduler сравнивает
process id текущего и следующего потока; при смене процесса активирует
целевой AS до `ArchContext::switch`.

Статические сегменты user-образа и стек загружает `userspace::load_user_image`.
Динамические user-VM mapping'и обслуживаются `UserVmAllocator` текущего
процесса и описаны в [`syscalls.md`](syscalls.md#memory).

## Boot protocol

Граница bootloader ↔ kernel — `hal-common::boot::BootInfo` с
`HwDescription::{Fdt, Acpi}`. `boot_main` работает с `BootInfo`, а не с
конкретным boot-протоколом.

`boot-linux-arm64` — текущий рабочий протокол. Он используется для
`linux_arm64`, `android_boot_v1` и `android_boot_v2`; различается только
упаковка образа в `xtask`. `boot-uefi` зарезервирован, но не реализован:
`xtask` и `build.rs` завершаются ошибкой для `uefi`.

`DEVICE_SPEC` выбирает device YAML; `build.rs` прокидывает `KERNEL_OFFSET`
в линкер и выбирает linker script по `boot.format`.
