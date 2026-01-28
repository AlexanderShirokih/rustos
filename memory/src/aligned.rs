pub trait Address: Copy + Clone + PartialOrd {
    fn as_usize(self) -> usize;

    fn as_u64(self) -> u64 {
        self.as_usize() as u64
    }
}

pub trait Aligned {
    const ALIGNMENT: usize;
}
