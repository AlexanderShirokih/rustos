//! Раздаёт потокам время процессоров и объявляет то, чем при этом распоряжается: процесс,
//! поток, очередь готовности, очередь ожидания.
//!
//! [`Scheduler`] проходит три состояния типом-параметром: [`Uninit`] до подъёма, [`Bootstrapped`]
//! после подъёма первого процессора, [`Running`] после входа в цикл. Каждый процессор поднимает
//! себя один раз через [`CpuEntry`]. Квант по умолчанию задан [`DEFAULT_QUANTUM_NS`], порядок
//! выбора - [`Priority`].
//!
//! [`SchedulerHandle`] - разделяемая ручка планировщика. Её берут слой syscall через трейт
//! [`SchedulerService`], крейт capability через свой `KernelRuntime`, подъём процесса - через
//! [`UserProcessLauncher`].
//!
//! Переключение контекста и таймер объявлены трейтами [`ArchContext`] и [`TimerSource`], а
//! реализует их платформенный слой. От архитектуры сам крейт не зависит.

#![cfg_attr(not(test), no_std)]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod address_space;
pub mod arch;
pub mod cpu;
pub mod process;
pub mod ready_queue;
pub mod scheduler;
pub mod service;
pub mod thread;
pub mod thread_table;
mod types;
pub mod user;
pub mod wait_queue;

pub use address_space::AddressSpace;
pub use arch::{
    ArchContext, ArchCpu, CpuId, CpuLocalError, MAX_CPUS, StackError, ThreadStack,
    ThreadStackAllocator, TimerSource, with_preemption_disabled,
};
pub use process::Process;
pub use scheduler::{
    Bootstrapped, CpuBootstrapError, DEFAULT_QUANTUM_NS, Running, Scheduler, SchedulerConfig,
    SchedulerStage, Uninit, lowest_priority,
};
pub use service::{
    CpuEntry, CurrentIpcContext, ORPHANED_EXIT_CODE, SchedulerHandle, UserProcessLauncher,
};
pub use syscall::ProcessName;
pub use thread::{Thread, ThreadState};
pub use types::{
    Priority, ProcessId, SchedulerService, SchedulerServiceExt, SpawnAddressSpace, SpawnConfig,
    SpawnError, ThreadId,
};
pub use user::{
    PreparedUserProcess, PreparedUserProcessError, UserBootstrapArg, UserEntry, UserProcessLaunch,
    UserProcessLaunchInfo,
};
