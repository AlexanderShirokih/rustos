//! Композиция rootkeeper: managed-компоненты, файловый namespace и supervisor.
#![no_std]

mod managed;
mod namespace;
pub mod relay;
mod system;

pub use managed::{
    BoundedStartupTransport, ManagedComponent, ManagedFailure, ManagedStartupError, ManagedStatus,
    RunningComponent,
};
pub use namespace::{
    Mount, MountFailure, NamespaceBootConfig, NamespaceBootError, NamespaceMountError,
    NamespaceRoots, launch_namespace, stop_namespace,
};
pub use system::{
    BOOT_MOUNTS, BootFailure, RootSystem, STOP_TIMEOUT, Stopped, boot_config, supervise,
};
