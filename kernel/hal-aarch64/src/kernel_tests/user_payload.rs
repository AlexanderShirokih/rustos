use kernel_tests::kernel_test;
use syscall::SyscallOp;

pub const fn svc_op(op: SyscallOp) -> Instruction {
    svc(op as u16)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Instruction(u32);

impl Instruction {
    pub const fn raw(word: u32) -> Self {
        Self(word)
    }

    pub const fn word(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Reg(u8);

impl Reg {
    pub const X0: Self = Self(0);
    pub const X1: Self = Self(1);
    pub const X2: Self = Self(2);
    pub const X19: Self = Self(19);
    pub const X20: Self = Self(20);
    pub const X21: Self = Self(21);

    const fn bits(self) -> u32 {
        self.0 as u32
    }
}

pub const B_LOOP: Instruction = Instruction::raw(0x1400_0000);

pub const fn svc(imm: u16) -> Instruction {
    Instruction::raw(0xD400_0001 | ((imm as u32) << 5))
}

pub const fn mov_x(rd: Reg, rm: Reg) -> Instruction {
    Instruction::raw(0xAA00_03E0 | (rm.bits() << 16) | rd.bits())
}

pub const fn movz_x(rd: Reg, imm: u16, shift16: u32) -> Instruction {
    assert!(shift16 < 4);
    Instruction::raw(0xD280_0000 | (shift16 << 21) | ((imm as u32) << 5) | rd.bits())
}

pub const fn movz_w(rd: Reg, imm: u16) -> Instruction {
    Instruction::raw(0x5280_0000 | ((imm as u32) << 5) | rd.bits())
}

pub const fn strb_w(rt: Reg, rn: Reg) -> Instruction {
    Instruction::raw(0x3900_0000 | (rn.bits() << 5) | rt.bits())
}

pub const fn cmp_x(rn: Reg, rm: Reg) -> Instruction {
    Instruction::raw(0xEB00_001F | (rm.bits() << 16) | (rn.bits() << 5))
}

pub const fn b_ne(disp_words: u32) -> Instruction {
    Instruction::raw(0x5400_0001 | ((disp_words & 0x7_FFFF) << 5))
}

pub fn words_to_bytes<const WORDS: usize, const BYTES: usize>(
    words: [Instruction; WORDS],
) -> [u8; BYTES] {
    assert!(BYTES == WORDS * 4, "payload byte length mismatch");

    let mut bytes = [0u8; BYTES];
    for (idx, instruction) in words.iter().enumerate() {
        let start = idx * 4;
        bytes[start..start + 4].copy_from_slice(&instruction.word().to_le_bytes());
    }
    bytes
}

#[kernel_test]
fn user_payload_encodes_known_words() {
    kernel_tests::kassert_eq!(svc(0x10).word(), 0xD400_0201);
    kernel_tests::kassert_eq!(mov_x(Reg::X21, Reg::X0).word(), 0xAA00_03F5);
    kernel_tests::kassert_eq!(movz_x(Reg::X1, 1, 0).word(), 0xD280_0021);
    kernel_tests::kassert_eq!(movz_w(Reg::X20, 0x42).word(), 0x5280_0854);
    kernel_tests::kassert_eq!(strb_w(Reg::X20, Reg::X19).word(), 0x3900_0274);
    kernel_tests::kassert_eq!(b_ne(7).word(), 0x5400_00E1);
}

#[kernel_test]
fn user_payload_serializes_words_as_le_bytes() {
    let bytes = words_to_bytes::<2, 8>([Instruction::raw(0x1122_3344), B_LOOP]);
    kernel_tests::kassert_eq!(bytes, [0x44, 0x33, 0x22, 0x11, 0, 0, 0, 0x14]);
}
