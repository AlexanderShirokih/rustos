//! Вычисление ordinal операции из канонического имени.
//!
//! Каноническое имя - `"<protocol>.<operation>"` в UTF-8. ordinal - младшие
//! 64 бита SHA-256: дайджест трактуется как big-endian 256-битное число,
//! младшие 64 бита суть последние 8 байт, прочитанные big-endian.

use sha2::{Digest, Sha256};

/// Каноническое имя операции протокола.
pub fn canonical_name(protocol: &str, operation: &str) -> String {
    let mut name = String::with_capacity(protocol.len() + operation.len() + 1);
    name.push_str(protocol);
    name.push('.');
    name.push_str(operation);
    name
}

/// ordinal канонического имени: младшие 64 бита SHA-256 (big-endian число).
pub fn ordinal_of(name: &str) -> u64 {
    let digest = Sha256::digest(name.as_bytes());
    let tail: [u8; 8] = digest[24..32]
        .try_into()
        .expect("SHA-256 yields 32 bytes, tail of 8");
    u64::from_be_bytes(tail)
}

#[cfg(test)]
mod tests {
    use super::{canonical_name, ordinal_of};

    #[test]
    fn canonical_name_joins_with_dot() {
        assert_eq!(canonical_name("Clock", "now"), "Clock.now");
    }

    #[test]
    fn ordinal_golden_values() {
        // Golden: младшие 64 бита SHA-256 канонического имени. Часть ABI.
        assert_eq!(ordinal_of("Clock.now"), 0x6ca5_80d0_d09a_d48b);
        assert_eq!(ordinal_of("Clock.resolution"), 0xa846_34a0_4ab6_2111);
        assert_eq!(ordinal_of("Clock.tick"), 0x25dd_513f_7649_54ce);
        assert_eq!(ordinal_of("Echo.ping"), 0xb3c6_5cdd_e9fd_a4db);
        assert_eq!(ordinal_of("Calc.add"), 0xb744_87cd_9b28_41ba);
        assert_eq!(ordinal_of("Calc.div"), 0x2420_7b95_a0d9_84fc);
        assert_eq!(ordinal_of("Calc.reset"), 0x1cb8_08a6_5161_cd40);
        assert_eq!(ordinal_of("Sensor.sample"), 0xc209_eb8f_0ef0_1937);
    }

    #[test]
    fn ordinal_depends_only_on_name() {
        // одно имя - один ordinal независимо от вызова.
        assert_eq!(ordinal_of("Clock.now"), ordinal_of("Clock.now"));
        assert_ne!(ordinal_of("Clock.now"), ordinal_of("Clock.resolution"));
    }

    #[test]
    fn namespace_changes_ordinal() {
        // Разный протокол-неймспейс даёт разный ordinal при том же операционном имени.
        assert_ne!(ordinal_of("Clock.tick"), ordinal_of("Timer.tick"));
    }
}
