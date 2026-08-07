/// Граница длины пути записи конфигурации в байтах.
pub const COMPONENT_CONFIG_MAX: usize = 255;

/// Путь записи конфигурации в собственном буфере: он переживает кадр, из
/// которого пришёл.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentConfig {
    bytes: [u8; COMPONENT_CONFIG_MAX],
    len: u8,
}

impl ComponentConfig {
    /// Копирует непустой путь в собственный буфер.
    #[must_use]
    pub fn new(path: &str) -> Option<Self> {
        let len = u8::try_from(path.len()).ok()?;
        if path.is_empty() {
            return None;
        }
        let mut bytes = [0; COMPONENT_CONFIG_MAX];
        bytes[..path.len()].copy_from_slice(path.as_bytes());
        Some(Self { bytes, len })
    }

    /// Путь записи конфигурации.
    #[must_use]
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_path_keeps_its_text() {
        let path = "/boot/namespace.mounts";
        let config = ComponentConfig::new(path).expect("path fits the buffer");
        assert_eq!(config.as_str(), path);
    }

    #[test]
    fn empty_and_overlong_paths_are_refused() {
        assert_eq!(ComponentConfig::new(""), None);
        let overlong = "/".repeat(COMPONENT_CONFIG_MAX + 1);
        assert_eq!(ComponentConfig::new(&overlong), None);
    }
}
