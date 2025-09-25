pub trait Device {
    fn init(&self);
    fn uninit(&self);
}
