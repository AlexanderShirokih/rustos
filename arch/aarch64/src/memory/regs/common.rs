pub enum EL0 {}
pub enum EL1 {}

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
