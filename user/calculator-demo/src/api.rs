#[ipc::protocol(name = "Calculator")]
pub trait CalculatorApi {
    #[call]
    fn multiply(&self, a: u64, b: u64) -> u64;
}
