use alloc::sync::Arc;

use memory::MemoryRegion;

use super::{
    port::Port, process::ProcessObject, reply::Reply, resource::Resource, signal::Signal,
    thread::ThreadObject,
};

/// capability target (capability target) - единица, к которой ядро выдаёт права.
pub enum CapabilityTarget {
    Signal(Arc<Signal>),
    Process(Arc<ProcessObject>),
    Thread(Arc<ThreadObject>),
    Memory(Arc<MemoryRegion>),
    Resource(Arc<Resource>),
    Port(Arc<Port>),
    Reply(Arc<Reply>),
}

impl Clone for CapabilityTarget {
    fn clone(&self) -> Self {
        match self {
            Self::Signal(s) => Self::Signal(s.clone()),
            Self::Process(p) => Self::Process(p.clone()),
            Self::Thread(t) => Self::Thread(t.clone()),
            Self::Memory(m) => Self::Memory(m.clone()),
            Self::Resource(r) => Self::Resource(r.clone()),
            Self::Port(e) => Self::Port(e.clone()),
            Self::Reply(r) => Self::Reply(r.clone()),
        }
    }
}

impl core::fmt::Debug for CapabilityTarget {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let name = match self {
            Self::Signal(_) => "Signal",
            Self::Process(_) => "Process",
            Self::Thread(_) => "Thread",
            Self::Memory(_) => "Memory",
            Self::Resource(_) => "Resource",
            Self::Port(_) => "Port",
            Self::Reply(_) => "Reply",
        };
        f.debug_struct(name).finish_non_exhaustive()
    }
}
