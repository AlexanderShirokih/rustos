//! Общие типы и макросы для системных регистров.

/// Маркер уровня исключения EL1.
pub enum EL1 {}

/// Объединяет биты из массива в одно значение u64.
#[macro_export]
macro_rules! combine_bits {
    ($bits:expr $(,)?) => {{
        let bits = $bits;
        let mut out: u64 = 0;
        let mut i: usize = 0;
        while i < bits.len() {
            out |= bits[i].encode();
            i += 1;
        }
        out
    }};
}
