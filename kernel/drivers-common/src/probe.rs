use alloc::{boxed::Box, string::String, vec::Vec};
use core::fmt;

use crate::{DeviceNode, driver::DriverFactory};

/// Результат пробирования драйвера.
pub type ProbeResult = Result<Box<dyn DriverFactory>, ProbeError>;

/// Ошибки при пробировании устройства.
#[derive(Debug)]
pub enum ProbeError {
    DriverCreationFailed(String),

    /// Отсутствует обязательное свойство.
    MissingProperty(&'static str),
    /// Устройство или функция не поддерживается.
    Unsupported(&'static str),
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProbeError::DriverCreationFailed(msg) => write!(f, "Failed to create driver: {msg}"),
            ProbeError::MissingProperty(prop) => write!(f, "Missing property '{prop}'"),
            ProbeError::Unsupported(feature) => write!(f, "Unsupported: {feature}"),
        }
    }
}

/// Контекст пробирования устройства для произвольного типа узла
pub struct ProbeContext<N: DeviceNode> {
    pub(crate) node: N,
    pub(crate) hierarchy: Vec<N>,
    pub(crate) console_node: Option<N::Id>,
}

impl<N: DeviceNode> ProbeContext<N> {
    pub fn new(node: N, hierarchy: Vec<N>, console_node: Option<N::Id>) -> Self {
        Self {
            node,
            hierarchy,
            console_node,
        }
    }

    pub fn node(&self) -> &N {
        &self.node
    }

    /// `true`, если загрузчик назначил консолью этот узел либо не назначил ни одного.
    pub fn is_console(&self) -> bool {
        self.console_node
            .as_ref()
            .is_none_or(|designated| *designated == self.node.id())
    }

    pub fn hierarchy(&self) -> &[N] {
        &self.hierarchy
    }

    pub fn parent(&self, node: &N) -> Option<&N> {
        self.hierarchy
            .iter()
            .enumerate()
            .find(|(_, n)| n.id() == node.id())
            .and_then(|(index, _)| index.checked_sub(1))
            .and_then(|index| self.hierarchy.get(index))
    }

    pub fn fold<F, S>(&self, fold: F) -> S
    where
        F: Fn(Option<&N>, &N, &ProbeContext<N>) -> Option<S>,
        S: core::iter::Sum,
    {
        self.hierarchy
            .iter()
            .enumerate()
            .filter_map(|(index, node)| {
                let parent = index.checked_sub(1).and_then(|idx| self.hierarchy.get(idx));
                fold(parent, node, self)
            })
            .sum::<S>()
    }
}
