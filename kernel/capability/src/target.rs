use alloc::sync::Arc;

use memory::MemoryRegion;

use super::{
    irq_control::IrqControl, irq_line::IrqLine, port::Port, process::ProcessObject, reply::Reply,
    resource::Resource, signal::Signal, thread::ThreadObject, wait::Waitable,
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
    IrqControl(Arc<IrqControl>),
    IrqLine(Arc<IrqLine>),
}

impl CapabilityTarget {
    /// Источник [`Waitable`], на котором можно ждать этот объект, если он
    /// ожидаем. Wait-путь спрашивает объект, а не матчит его тип, поэтому
    /// знание «какой объект ожидаем и каким сигналом» живёт здесь, рядом с
    /// определением вариантов, а не в `signal_wait_many`.
    ///
    /// `Process`/`Thread` лениво материализуют bound-`Signal` терминации
    /// (как делал прежний `*_termination_signal`): отдельный `Signal`-хендл
    /// при этом не создаётся, поэтому подделать событие через `signal_set`
    /// нельзя — `signal_set` строго принимает только `Signal`.
    pub(crate) fn as_waitable(&self) -> Option<Arc<dyn Waitable>> {
        match self {
            Self::Signal(s) => Some(s.clone() as Arc<dyn Waitable>),
            Self::Process(p) => Some(p.termination_signal() as Arc<dyn Waitable>),
            Self::Thread(t) => Some(t.termination_signal() as Arc<dyn Waitable>),
            Self::IrqLine(l) => Some(l.event_signal() as Arc<dyn Waitable>),
            Self::Memory(_)
            | Self::Resource(_)
            | Self::Port(_)
            | Self::Reply(_)
            | Self::IrqControl(_) => None,
        }
    }
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
            Self::IrqControl(c) => Self::IrqControl(c.clone()),
            Self::IrqLine(l) => Self::IrqLine(l.clone()),
        }
    }
}

impl From<Arc<Signal>> for CapabilityTarget {
    fn from(obj: Arc<Signal>) -> Self {
        Self::Signal(obj)
    }
}

impl From<Arc<ProcessObject>> for CapabilityTarget {
    fn from(obj: Arc<ProcessObject>) -> Self {
        Self::Process(obj)
    }
}

impl From<Arc<ThreadObject>> for CapabilityTarget {
    fn from(obj: Arc<ThreadObject>) -> Self {
        Self::Thread(obj)
    }
}

impl From<Arc<MemoryRegion>> for CapabilityTarget {
    fn from(obj: Arc<MemoryRegion>) -> Self {
        Self::Memory(obj)
    }
}

impl From<Arc<Resource>> for CapabilityTarget {
    fn from(obj: Arc<Resource>) -> Self {
        Self::Resource(obj)
    }
}

impl From<Arc<Port>> for CapabilityTarget {
    fn from(obj: Arc<Port>) -> Self {
        Self::Port(obj)
    }
}

impl From<Arc<Reply>> for CapabilityTarget {
    fn from(obj: Arc<Reply>) -> Self {
        Self::Reply(obj)
    }
}

impl From<Arc<IrqControl>> for CapabilityTarget {
    fn from(obj: Arc<IrqControl>) -> Self {
        Self::IrqControl(obj)
    }
}

impl From<Arc<IrqLine>> for CapabilityTarget {
    fn from(obj: Arc<IrqLine>) -> Self {
        Self::IrqLine(obj)
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
            Self::IrqControl(_) => "IrqControl",
            Self::IrqLine(_) => "IrqLine",
        };
        f.debug_struct(name).finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_waitable_some_for_signalable_objects() {
        assert!(
            CapabilityTarget::Signal(Signal::new())
                .as_waitable()
                .is_some()
        );
        assert!(
            CapabilityTarget::Process(ProcessObject::new())
                .as_waitable()
                .is_some()
        );
        assert!(
            CapabilityTarget::Thread(ThreadObject::new())
                .as_waitable()
                .is_some()
        );
    }

    #[test]
    fn as_waitable_none_for_non_waitable_objects() {
        assert!(CapabilityTarget::Port(Port::new()).as_waitable().is_none());
    }

    #[test]
    fn as_waitable_on_terminated_process_is_presignaled() {
        let process = ProcessObject::new();
        process.signal_terminated(0);
        let waitable = CapabilityTarget::Process(process)
            .as_waitable()
            .expect("process is waitable");
        assert_eq!(
            waitable.peek() & super::super::signal::SIGNALED,
            super::super::signal::SIGNALED
        );
    }
}
