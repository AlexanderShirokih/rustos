use fdt::devicetreeext::PropExt;
use kernel_core::driver::early::ProbeContext;

pub(crate) trait ProbeContextExt {
    fn reg_offset(&self) -> usize;
}

impl ProbeContextExt for ProbeContext<'_> {
    fn reg_offset(&self) -> usize {
        self.fold(|node, context| {
            node.prop("reg")
                .and_then(|reg| reg.try_as_offset_size(context.cells_size()))
                .map(|offset_size| offset_size.offset)
        })
    }
}
