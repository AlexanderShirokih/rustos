use alloc::sync::Arc;

use memory::MemoryRegion;

use super::{
    koid::Koid, port::Port, process::ProcessObject, reply::Reply, resource::Resource,
    signal::Signal, thread::ThreadObject,
};

/// Kernel-объект (KO) - единица, к которой ядро выдаёт права.
pub enum KObject {
    Signal(Arc<Signal>),
    Process(Arc<ProcessObject>),
    Thread(Arc<ThreadObject>),
    Memory(Arc<MemoryRegion>),
    Resource(Arc<Resource>),
    Port(Arc<Port>),
    Reply(Arc<Reply>),
}

impl KObject {
    /// Уникальный идентификатор объекта.
    pub fn koid(&self) -> Koid {
        match self {
            Self::Signal(s) => Koid::from_parts(self.type_tag(), Arc::as_ptr(s) as u64),
            Self::Process(p) => Koid::from_parts(self.type_tag(), Arc::as_ptr(p) as u64),
            Self::Thread(t) => Koid::from_parts(self.type_tag(), Arc::as_ptr(t) as u64),
            Self::Memory(m) => Koid::from_parts(self.type_tag(), Arc::as_ptr(m) as u64),
            Self::Resource(r) => Koid::from_parts(self.type_tag(), Arc::as_ptr(r) as u64),
            Self::Port(e) => Koid::from_parts(self.type_tag(), Arc::as_ptr(e) as u64),
            Self::Reply(r) => Koid::from_parts(self.type_tag(), Arc::as_ptr(r) as u64),
        }
    }

    fn type_tag(&self) -> u8 {
        match self {
            Self::Signal(_) => 1,
            Self::Process(_) => 2,
            Self::Thread(_) => 3,
            Self::Memory(_) => 4,
            Self::Resource(_) => 5,
            Self::Port(_) => 6,
            Self::Reply(_) => 7,
        }
    }
}

impl Clone for KObject {
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

impl core::fmt::Debug for KObject {
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
        f.debug_struct(name).field("koid", &self.koid()).finish()
    }
}
