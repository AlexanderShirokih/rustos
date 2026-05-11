pub const BOOTSTRAP_HELLO_MAGIC: [u8; 8] = *b"RKHELLO\0";
pub const BOOTSTRAP_ABI_VERSION: u16 = 1;
pub const BOOTSTRAP_HELLO_SIZE: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootstrapHello {
    pub magic: [u8; 8],
    pub version: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapHelloError {
    PayloadTooShort { needed: usize, actual: usize },
    InvalidMagic([u8; 8]),
    InvalidVersion(u16),
}

pub fn parse_bootstrap_hello(bytes: &[u8]) -> Result<BootstrapHello, BootstrapHelloError> {
    if bytes.len() < BOOTSTRAP_HELLO_SIZE {
        return Err(BootstrapHelloError::PayloadTooShort {
            needed: BOOTSTRAP_HELLO_SIZE,
            actual: bytes.len(),
        });
    }

    let hello = BootstrapHello {
        magic: bytes[0..8].try_into().expect("hello magic is 8 bytes"),
        version: u16::from_le_bytes(bytes[8..10].try_into().expect("hello version is 2 bytes")),
    };

    if hello.magic != BOOTSTRAP_HELLO_MAGIC {
        return Err(BootstrapHelloError::InvalidMagic(hello.magic));
    }
    if hello.version != BOOTSTRAP_ABI_VERSION {
        return Err(BootstrapHelloError::InvalidVersion(hello.version));
    }

    Ok(hello)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_hello() {
        let mut bytes = [0u8; BOOTSTRAP_HELLO_SIZE];
        bytes[0..8].copy_from_slice(&BOOTSTRAP_HELLO_MAGIC);
        bytes[8..10].copy_from_slice(&BOOTSTRAP_ABI_VERSION.to_le_bytes());

        let hello = parse_bootstrap_hello(&bytes).expect("hello should parse");
        assert_eq!(hello.magic, BOOTSTRAP_HELLO_MAGIC);
        assert_eq!(hello.version, BOOTSTRAP_ABI_VERSION);
    }

    #[test]
    fn rejects_short_hello() {
        let bytes = [0u8; 8];
        let err = parse_bootstrap_hello(&bytes).expect_err("short hello must fail");
        assert_eq!(
            err,
            BootstrapHelloError::PayloadTooShort {
                needed: BOOTSTRAP_HELLO_SIZE,
                actual: 8,
            }
        );
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = [0u8; BOOTSTRAP_HELLO_SIZE];
        bytes[0..8].copy_from_slice(b"BADMAGIC");
        bytes[8..10].copy_from_slice(&BOOTSTRAP_ABI_VERSION.to_le_bytes());

        let err = parse_bootstrap_hello(&bytes).expect_err("bad magic must fail");
        assert!(matches!(err, BootstrapHelloError::InvalidMagic(_)));
    }

    #[test]
    fn rejects_bad_version() {
        let mut bytes = [0u8; BOOTSTRAP_HELLO_SIZE];
        bytes[0..8].copy_from_slice(&BOOTSTRAP_HELLO_MAGIC);
        bytes[8..10].copy_from_slice(&2u16.to_le_bytes());

        let err = parse_bootstrap_hello(&bytes).expect_err("bad version must fail");
        assert_eq!(err, BootstrapHelloError::InvalidVersion(2));
    }
}
