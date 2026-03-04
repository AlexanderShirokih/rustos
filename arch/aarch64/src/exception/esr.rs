/// Exception Syndrome Register - содержит информацию о причине исключения.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Esr(u64);

impl Esr {
    pub const fn raw(self) -> u64 {
        self.0
    }

    pub const fn exception_class(self) -> ExceptionClass {
        let ec = ((self.0 >> 26) & 0x3F) as u8;
        ExceptionClass::from_raw(ec)
    }

    pub const fn iss(self) -> u32 {
        (self.0 & 0x1FF_FFFF) as u32
    }
}

/// Exception Class - тип синхронного исключения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExceptionClass {
    /// Unknown reason.
    Unknown,
    /// Trapped WFI or WFE instruction execution.
    TrappedWfiWfe,
    /// Illegal Execution state.
    IllegalExecutionState,
    /// SVC instruction execution in AArch64 state.
    Svc,
    /// Trapped MSR, MRS or System instruction execution.
    SystemInstruction,
    /// Instruction Abort from a lower Exception level.
    InstructionAbortLower,
    /// Instruction Abort taken without a change in Exception level.
    InstructionAbortSame,
    /// PC alignment fault exception.
    PcAlignment,
    /// Data Abort from a lower Exception level.
    DataAbortLower,
    /// Data Abort taken without a change in Exception level.
    DataAbortSame,
    /// SP alignment fault exception.
    SpAlignment,
    /// Breakpoint exception from a lower Exception level.
    BreakpointLower,
    /// Breakpoint exception taken without a change in Exception level.
    BreakpointSame,
    /// Software Step exception from a lower Exception level.
    SoftwareStepLower,
    /// Software Step exception taken without a change in Exception level.
    SoftwareStepSame,
    /// Watchpoint exception from a lower Exception level.
    WatchpointLower,
    /// Watchpoint exception taken without a change in Exception level.
    WatchpointSame,
    /// BKPT instruction execution in AArch32 state.
    BkptAarch32,
    /// BRK instruction execution in AArch64 state.
    BrkAarch64,
    /// Other exception class not explicitly handled.
    Other(u8),
}

impl ExceptionClass {
    pub const fn from_raw(ec: u8) -> Self {
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

    pub const fn description(self) -> &'static str {
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
