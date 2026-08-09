ipc::protocol! {
    /// Демонстрационный контракт калькулятора.
    pub mod calculator {
        name = "Calculator";

        /// Перемножает два числа.
        call fn multiply(a: u64, b: u64) -> u64;
    }
}
