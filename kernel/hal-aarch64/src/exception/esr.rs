#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Esr(u64);

impl Esr {
    pub(crate) const fn raw(self) -> u64 {
        self.0
    }

    pub(crate) const fn exception_class(self) -> ExceptionClass {
        let ec = ((self.0 >> 26) & 0x3F) as u8;
        ExceptionClass::from_raw(ec)
    }

    pub(crate) const fn iss(self) -> u32 {
        (self.0 & 0x1FF_FFFF) as u32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExceptionClass {
    Unknown,
    TrappedWfiWfe,
    IllegalExecutionState,
    Svc,
    SystemInstruction,
    InstructionAbortLower,
    InstructionAbortSame,
    PcAlignment,
    DataAbortLower,
    DataAbortSame,
    SpAlignment,
    BreakpointLower,
    BreakpointSame,
    SoftwareStepLower,
    SoftwareStepSame,
    WatchpointLower,
    WatchpointSame,
    BkptAarch32,
    BrkAarch64,
    Other(u8),
}

impl ExceptionClass {
    pub(crate) const fn from_raw(ec: u8) -> Self {
        match ec {
            0x00 => Self::Unknown,
            0x01 => Self::TrappedWfiWfe,
            0x0E => Self::IllegalExecutionState,
            0x15 => Self::Svc,
            0x18 => Self::SystemInstruction,
            0x20 => Self::InstructionAbortLower,
            0x21 => Self::InstructionAbortSame,
            0x22 => Self::PcAlignment,
            0x24 => Self::DataAbortLower,
            0x25 => Self::DataAbortSame,
            0x26 => Self::SpAlignment,
            0x30 => Self::BreakpointLower,
            0x31 => Self::BreakpointSame,
            0x32 => Self::SoftwareStepLower,
            0x33 => Self::SoftwareStepSame,
            0x34 => Self::WatchpointLower,
            0x35 => Self::WatchpointSame,
            0x38 => Self::BkptAarch32,
            0x3C => Self::BrkAarch64,
            other => Self::Other(other),
        }
    }

    pub(crate) const fn description(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown reason",
            Self::TrappedWfiWfe => "Trapped WFI/WFE",
            Self::IllegalExecutionState => "Illegal Execution state",
            Self::Svc => "SVC (AArch64)",
            Self::SystemInstruction => "MSR/MRS/System instruction trap",
            Self::InstructionAbortLower => "Instruction Abort (lower EL)",
            Self::InstructionAbortSame => "Instruction Abort (same EL)",
            Self::PcAlignment => "PC alignment fault",
            Self::DataAbortLower => "Data Abort (lower EL)",
            Self::DataAbortSame => "Data Abort (same EL)",
            Self::SpAlignment => "SP alignment fault",
            Self::BreakpointLower => "Breakpoint (lower EL)",
            Self::BreakpointSame => "Breakpoint (same EL)",
            Self::SoftwareStepLower => "Software Step (lower EL)",
            Self::SoftwareStepSame => "Software Step (same EL)",
            Self::WatchpointLower => "Watchpoint (lower EL)",
            Self::WatchpointSame => "Watchpoint (same EL)",
            Self::BkptAarch32 => "BKPT (AArch32)",
            Self::BrkAarch64 => "BRK (AArch64)",
            Self::Other(_) => "Other",
        }
    }
}
