use alloc::sync::Arc;
use core::num::NonZeroU64;

use memory::MemoryRegion;
use syscall::Rights;

use super::{
    errors::IpcError, irq_control::IrqControl, irq_line::IrqLine, port::Port,
    process::ProcessObject, reply::Reply, resource::Resource, rev_node::RevocationHook,
    signal::Signal, thread::ThreadObject, wait::Waitable,
};

pub(crate) trait HolderTicket: RevocationHook {
    fn activate(&self) -> Result<(), IpcError>;

    fn release(&self);

    fn reacquire(&self) -> Option<Arc<dyn HolderTicket>>;
}

/// Удержание учёта стороны объекта на время операции. Снимается по `Drop`. Учёт держится
/// независимо от capability, от которой взято удержание, и сохраняется после её закрытия.
#[must_use = "OperationHold keeps the object side counted until the operation ends"]
pub struct OperationHold(Arc<dyn HolderTicket>);

impl OperationHold {
    pub(crate) fn new(ticket: Arc<dyn HolderTicket>) -> Self {
        Self(ticket)
    }
}

impl Drop for OperationHold {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// Объект ядра, на который выдаётся [`Capability`](super::Capability). Права и значок хендла
/// применяются к этому объекту.
///
/// Клон варианта делит объект с оригиналом. Вариант задаёт и набор применимых операций. Ожидание
/// сигналов доступно на `Signal`, `Process`, `Thread` и `IrqLine`. `Port` и `Process` ведут учёт
/// держателей.
#[derive(Clone)]
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

/// Объект ядра, извлекаемый из своего варианта [`CapabilityTarget`]. Обратен `From<Arc<T>>`,
/// которым объект в этот вариант заворачивается.
///
/// Реализация задаёт пару "тип - вариант", по которой
/// [`HandleTable::get_object`](super::HandleTable::get_object) проверяет тип хендла и отвергает
/// чужой с [`IpcError::WrongType`](super::IpcError::WrongType).
pub trait TargetObject: Send + Sync {
    /// Объект этого типа внутри `target`. `None` - вариант другого вида.
    fn from_target(target: &CapabilityTarget) -> Option<&Arc<Self>>;
}

macro_rules! impl_target_object {
    ($($object:ty => $variant:ident,)+) => {$(
        impl TargetObject for $object {
            fn from_target(target: &CapabilityTarget) -> Option<&Arc<Self>> {
                if let CapabilityTarget::$variant(object) = target {
                    Some(object)
                } else {
                    None
                }
            }
        }
    )+};
}

impl_target_object! {
    Signal => Signal,
    ProcessObject => Process,
    ThreadObject => Thread,
    MemoryRegion => Memory,
    Resource => Resource,
    Port => Port,
    Reply => Reply,
    IrqControl => IrqControl,
    IrqLine => IrqLine,
}

impl CapabilityTarget {
    pub(crate) fn as_waitable(&self) -> Option<Arc<dyn Waitable>> {
        match self {
            Self::Signal(s) => Some(s.clone() as Arc<dyn Waitable>),
            // Bound-сигнал терминации хендлом не выдаётся: `signal_set` принимает только
            // `Signal`, поэтому событие терминации из userspace не подделывается.
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

    pub(crate) fn holder_ticket(&self, rights: Rights) -> Option<Arc<dyn HolderTicket>> {
        match self {
            Self::Port(port) => port.holder_ticket(rights),
            Self::Process(process) => process.holder_ticket(rights),
            Self::Signal(_)
            | Self::Thread(_)
            | Self::Memory(_)
            | Self::Resource(_)
            | Self::Reply(_)
            | Self::IrqControl(_)
            | Self::IrqLine(_) => None,
        }
    }

    pub(crate) fn counted_holder_ticket(&self, rights: Rights) -> Option<Arc<dyn HolderTicket>> {
        match self {
            Self::Port(port) => port.counted_holder_ticket(rights),
            Self::Process(process) => process.counted_holder_ticket(rights),
            Self::Signal(_)
            | Self::Thread(_)
            | Self::Memory(_)
            | Self::Resource(_)
            | Self::Reply(_)
            | Self::IrqControl(_)
            | Self::IrqLine(_) => None,
        }
    }

    pub(crate) fn badged_holder_ticket(
        &self,
        rights: Rights,
        badge: NonZeroU64,
    ) -> Option<Arc<dyn HolderTicket>> {
        match self {
            Self::Port(port) => port.badged_holder_ticket(rights, badge),
            Self::Process(process) => process.holder_ticket(rights),
            Self::Signal(_)
            | Self::Thread(_)
            | Self::Memory(_)
            | Self::Resource(_)
            | Self::Reply(_)
            | Self::IrqControl(_)
            | Self::IrqLine(_) => None,
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
    fn holders_are_counted_by_ports_and_processes() {
        let ticket = CapabilityTarget::Port(Port::new())
            .holder_ticket(Rights::READ)
            .expect("port counts holders");
        assert!(ticket.activate().is_ok());
        assert!(
            CapabilityTarget::Process(ProcessObject::new())
                .holder_ticket(Rights::WRITE)
                .is_some()
        );
        assert!(
            CapabilityTarget::Signal(Signal::new())
                .holder_ticket(Rights::READ)
                .is_none()
        );
    }

    #[test]
    fn process_holder_needs_the_write_right() {
        let process = CapabilityTarget::Process(ProcessObject::new());
        assert!(process.holder_ticket(Rights::READ).is_none());
        assert!(process.counted_holder_ticket(Rights::READ).is_none());
    }

    #[test]
    fn badged_holders_are_counted_by_ports_and_processes() {
        let badge = NonZeroU64::MIN;
        let ticket = CapabilityTarget::Port(Port::new())
            .badged_holder_ticket(Rights::WRITE, badge)
            .expect("port counts badged holders");
        assert!(ticket.activate().is_ok());
        assert!(
            CapabilityTarget::Process(ProcessObject::new())
                .badged_holder_ticket(Rights::WRITE, badge)
                .is_some()
        );
        assert!(
            CapabilityTarget::Signal(Signal::new())
                .badged_holder_ticket(Rights::WRITE, badge)
                .is_none()
        );
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
