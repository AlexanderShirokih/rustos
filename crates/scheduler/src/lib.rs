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
    ArchContext, ArchCpu, CpuId, StackError, ThreadStack, ThreadStackAllocator, TimerSource,
    with_preemption_disabled,
};
pub use process::Process;
pub use scheduler::{
    Bootstrapped, Running, Scheduler, SchedulerConfig, SchedulerStage, Uninit, lowest_priority,
};
pub use service::{SchedulerHandle, UserProcessLauncher};
pub use thread::{Thread, ThreadState};
pub use types::{
    Priority, ProcessId, SchedulerService, SchedulerServiceExt, SpawnAddressSpace, SpawnConfig,
    SpawnError, ThreadId,
};
pub use user::{
    PreparedUserProcess, PreparedUserProcessError, UserBootstrapArg, UserEntry, UserProcessLaunch,
    UserProcessLaunchInfo,
};
