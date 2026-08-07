//! Ограничения `Resource` в syscall ABI.

/// Потолок глубины цепочки `split`: `ResourceSplit` на этой глубине отказывает.
pub const MAX_RESOURCE_DEPTH: u16 = 8;
