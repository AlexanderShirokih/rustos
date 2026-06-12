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

pub const BOOTSTRAP_LOG_MAGIC: [u8; 8] = *b"RKLOG\0\0\0";
pub const BOOTSTRAP_LOG_HEADER_SIZE: usize = 8;
/// Лимит utf8-payload лог-кадра: кадр целиком укладывается в максимальный
/// inline-размер сообщения канала (256 байт).
pub const BOOTSTRAP_LOG_PAYLOAD_MAX: usize = 248;
pub const BOOTSTRAP_LOG_FRAME_MAX: usize = BOOTSTRAP_LOG_HEADER_SIZE + BOOTSTRAP_LOG_PAYLOAD_MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapLogError {
    PayloadTooShort { needed: usize, actual: usize },
    InvalidMagic([u8; 8]),
    PayloadTooLong { max: usize, actual: usize },
}

/// Разбирает RKLOG-кадр и возвращает payload без магии.
pub fn parse_bootstrap_log(bytes: &[u8]) -> Result<&[u8], BootstrapLogError> {
    let Some(magic) = bytes.first_chunk::<BOOTSTRAP_LOG_HEADER_SIZE>() else {
        return Err(BootstrapLogError::PayloadTooShort {
            needed: BOOTSTRAP_LOG_HEADER_SIZE,
            actual: bytes.len(),
        });
    };
    if *magic != BOOTSTRAP_LOG_MAGIC {
        return Err(BootstrapLogError::InvalidMagic(*magic));
    }

    let payload = &bytes[BOOTSTRAP_LOG_HEADER_SIZE..];
    if payload.len() > BOOTSTRAP_LOG_PAYLOAD_MAX {
        return Err(BootstrapLogError::PayloadTooLong {
            max: BOOTSTRAP_LOG_PAYLOAD_MAX,
            actual: payload.len(),
        });
    }
    Ok(payload)
}

/// Кодирует RKLOG-кадр с `payload` в `frame`; возвращает длину кадра.
pub fn encode_bootstrap_log(
    payload: &[u8],
    frame: &mut [u8; BOOTSTRAP_LOG_FRAME_MAX],
) -> Result<usize, BootstrapLogError> {
    if payload.len() > BOOTSTRAP_LOG_PAYLOAD_MAX {
        return Err(BootstrapLogError::PayloadTooLong {
            max: BOOTSTRAP_LOG_PAYLOAD_MAX,
            actual: payload.len(),
        });
    }

    frame[..BOOTSTRAP_LOG_HEADER_SIZE].copy_from_slice(&BOOTSTRAP_LOG_MAGIC);
    frame[BOOTSTRAP_LOG_HEADER_SIZE..BOOTSTRAP_LOG_HEADER_SIZE + payload.len()]
        .copy_from_slice(payload);
    Ok(BOOTSTRAP_LOG_HEADER_SIZE + payload.len())
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

    #[test]
    fn round_trips_log() {
        for payload in [
            b"".as_slice(),
            b"hello",
            &[0xA5u8; BOOTSTRAP_LOG_PAYLOAD_MAX],
        ] {
            let mut frame = [0u8; BOOTSTRAP_LOG_FRAME_MAX];
            let len = encode_bootstrap_log(payload, &mut frame).expect("log should encode");
            assert_eq!(len, BOOTSTRAP_LOG_HEADER_SIZE + payload.len());

            let parsed = parse_bootstrap_log(&frame[..len]).expect("log should parse");
            assert_eq!(parsed, payload);
        }
    }

    #[test]
    fn rejects_short_log() {
        let bytes = [0u8; 4];
        let err = parse_bootstrap_log(&bytes).expect_err("short log must fail");
        assert_eq!(
            err,
            BootstrapLogError::PayloadTooShort {
                needed: BOOTSTRAP_LOG_HEADER_SIZE,
                actual: 4,
            }
        );
    }

    #[test]
    fn rejects_bad_log_magic() {
        let mut bytes = [0u8; BOOTSTRAP_LOG_HEADER_SIZE];
        bytes.copy_from_slice(b"BADMAGIC");

        let err = parse_bootstrap_log(&bytes).expect_err("bad magic must fail");
        assert_eq!(err, BootstrapLogError::InvalidMagic(*b"BADMAGIC"));
    }

    #[test]
    fn rejects_oversized_log_payload() {
        let payload = [0u8; BOOTSTRAP_LOG_PAYLOAD_MAX + 1];
        let mut frame = [0u8; BOOTSTRAP_LOG_FRAME_MAX];
        let err = encode_bootstrap_log(&payload, &mut frame).expect_err("oversized must fail");
        assert_eq!(
            err,
            BootstrapLogError::PayloadTooLong {
                max: BOOTSTRAP_LOG_PAYLOAD_MAX,
                actual: BOOTSTRAP_LOG_PAYLOAD_MAX + 1,
            }
        );

        let mut oversized = [0u8; BOOTSTRAP_LOG_FRAME_MAX + 1];
        oversized[0..8].copy_from_slice(&BOOTSTRAP_LOG_MAGIC);
        let err = parse_bootstrap_log(&oversized).expect_err("oversized frame must fail");
        assert_eq!(
            err,
            BootstrapLogError::PayloadTooLong {
                max: BOOTSTRAP_LOG_PAYLOAD_MAX,
                actual: BOOTSTRAP_LOG_PAYLOAD_MAX + 1,
            }
        );
    }

    #[test]
    fn log_parser_rejects_hello_magic() {
        let mut bytes = [0u8; BOOTSTRAP_LOG_HEADER_SIZE];
        bytes.copy_from_slice(&BOOTSTRAP_HELLO_MAGIC);

        let err = parse_bootstrap_log(&bytes).expect_err("hello magic must fail");
        assert_eq!(err, BootstrapLogError::InvalidMagic(BOOTSTRAP_HELLO_MAGIC));
    }

    #[test]
    fn hello_parser_rejects_log_magic() {
        let mut frame = [0u8; BOOTSTRAP_LOG_FRAME_MAX];
        let len = encode_bootstrap_log(b"0123456789", &mut frame).expect("log should encode");

        let err = parse_bootstrap_hello(&frame[..len]).expect_err("log frame must fail");
        assert_eq!(err, BootstrapHelloError::InvalidMagic(BOOTSTRAP_LOG_MAGIC));
    }
}
