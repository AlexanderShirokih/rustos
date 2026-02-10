use crate::probe::{NodeProbeExt, ProbeError, ProbeResult};
use crate::tree::DeviceNode;

impl<N: DeviceNode> NodeProbeExt for N {
    fn require_prop(&self, name: &'static str) -> ProbeResult<Self::Property<'_>> {
        self.prop(name).ok_or(ProbeError::MissingProperty(name))
    }
}
