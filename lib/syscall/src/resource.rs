//! Ограничения `Resource` в syscall ABI.

/// Потолок глубины цепочки `split`. `ResourceSplit` на ресурсе этой глубины отвергается
/// кодом `ResourceExhausted`, корневой ресурс имеет глубину `0`.
pub const MAX_RESOURCE_DEPTH: u16 = 8;
