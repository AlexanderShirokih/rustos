//! Окно регистров GCC, отображённое в процесс контроллера.

use clk_qcom::Registers;
use kernel_objects::MmioMapping;

pub(crate) struct Window(pub(crate) MmioMapping);

impl Registers for Window {
    fn size(&self) -> usize {
        self.0.size()
    }

    fn load(&self, offset: usize) -> u32 {
        self.0.mmio().read::<u32>(offset)
    }

    fn store(&self, offset: usize, value: u32) {
        self.0.mmio().write::<u32>(offset, value);
    }
}
