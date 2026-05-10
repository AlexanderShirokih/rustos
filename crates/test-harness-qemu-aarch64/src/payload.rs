//! Helpers for building tiny user-mode instruction payloads in QEMU tests.

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
    pub const X3: Self = Self(3);
    pub const X4: Self = Self(4);
    pub const X19: Self = Self(19);
    pub const X20: Self = Self(20);
    pub const X21: Self = Self(21);
    pub const X22: Self = Self(22);
    pub const X23: Self = Self(23);
    pub const X24: Self = Self(24);
    pub const X25: Self = Self(25);

    pub const fn new(index: u8) -> Self {
        assert!(index < 32);
        Self(index)
    }

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

pub const fn movk_x(rd: Reg, imm: u16, shift16: u32) -> Instruction {
    assert!(shift16 < 4);
    Instruction::raw(0xF280_0000 | (shift16 << 21) | ((imm as u32) << 5) | rd.bits())
}

pub const fn movz_w(rd: Reg, imm: u16) -> Instruction {
    Instruction::raw(0x5280_0000 | ((imm as u32) << 5) | rd.bits())
}

pub const fn str_x(rt: Reg, rn: Reg) -> Instruction {
    Instruction::raw(0xF900_0000 | (rn.bits() << 5) | rt.bits())
}

pub const fn str_w(rt: Reg, rn: Reg) -> Instruction {
    Instruction::raw(0xB900_0000 | (rn.bits() << 5) | rt.bits())
}

pub const fn strb_w(rt: Reg, rn: Reg) -> Instruction {
    Instruction::raw(0x3900_0000 | (rn.bits() << 5) | rt.bits())
}

pub const fn ldr_x(rt: Reg, rn: Reg) -> Instruction {
    Instruction::raw(0xF940_0000 | (rn.bits() << 5) | rt.bits())
}

pub const fn ldr_w(rt: Reg, rn: Reg) -> Instruction {
    Instruction::raw(0xB940_0000 | (rn.bits() << 5) | rt.bits())
}

pub const fn cmp_x(rn: Reg, rm: Reg) -> Instruction {
    Instruction::raw(0xEB00_001F | (rm.bits() << 16) | (rn.bits() << 5))
}

pub const fn cmp_x_imm12(rn: Reg, imm12: u16) -> Instruction {
    assert!(imm12 < 0x1000);
    Instruction::raw(0xF100_001F | ((imm12 as u32) << 10) | (rn.bits() << 5))
}

pub const fn b_ne(disp_words: u32) -> Instruction {
    Instruction::raw(0x5400_0001 | ((disp_words & 0x7_FFFF) << 5))
}

pub const fn cbz_x(rt: Reg, disp_words: u32) -> Instruction {
    Instruction::raw(0xB400_0000 | ((disp_words & 0x7_FFFF) << 5) | rt.bits())
}

pub const fn cbnz_x(rt: Reg, disp_words: u32) -> Instruction {
    Instruction::raw(0xB500_0000 | ((disp_words & 0x7_FFFF) << 5) | rt.bits())
}

pub const fn tbnz_x(rt: Reg, bit: u8, disp_words: u32) -> Instruction {
    assert!(bit < 64);
    let bit = bit as u32;
    let b5 = (bit >> 5) << 31;
    let b40 = (bit & 0x1F) << 19;
    Instruction::raw(0x3700_0000 | b5 | b40 | ((disp_words & 0x3FFF) << 5) | rt.bits())
}

pub struct Builder<const WORDS: usize> {
    words: [Instruction; WORDS],
    len: usize,
}

impl<const WORDS: usize> Builder<WORDS> {
    pub const fn new(fill: Instruction) -> Self {
        Self {
            words: [fill; WORDS],
            len: 0,
        }
    }

    pub fn push(&mut self, instruction: Instruction) {
        assert!(self.len < WORDS, "payload overflow");
        self.words[self.len] = instruction;
        self.len += 1;
    }

    pub fn extend<const N: usize>(&mut self, instructions: [Instruction; N]) {
        for instruction in instructions {
            self.push(instruction);
        }
    }

    pub fn reserve(&mut self, fill: Instruction) -> usize {
        let index = self.len;
        self.push(fill);
        index
    }

    pub fn set(&mut self, index: usize, instruction: Instruction) {
        assert!(index < WORDS, "payload patch out of bounds");
        self.words[index] = instruction;
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn assert_full(&self) {
        debug_assert_eq!(self.len, WORDS);
    }

    pub fn mov_u16(&mut self, rd: Reg, imm: u16) {
        self.push(movz_x(rd, imm, 0));
    }

    pub fn mov_u32_fixed(&mut self, rd: Reg, imm: u32) {
        self.push(movz_x(rd, imm as u16, 0));
        self.push(movk_x(rd, (imm >> 16) as u16, 1));
    }

    pub fn mov_u64_fixed(&mut self, rd: Reg, imm: u64) {
        self.push(movz_x(rd, imm as u16, 0));
        self.push(movk_x(rd, (imm >> 16) as u16, 1));
        self.push(movk_x(rd, (imm >> 32) as u16, 2));
        self.push(movk_x(rd, (imm >> 48) as u16, 3));
    }

    pub fn into_bytes<const BYTES: usize>(self) -> [u8; BYTES] {
        words_to_bytes(self.words)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_existing_payload_words() {
        assert_eq!(svc(0x10).word(), 0xD400_0201);
        assert_eq!(mov_x(Reg::X21, Reg::X0).word(), 0xAA00_03F5);
        assert_eq!(movz_x(Reg::X1, 1, 0).word(), 0xD280_0021);
        assert_eq!(movz_w(Reg::X20, 0x42).word(), 0x5280_0854);
        assert_eq!(strb_w(Reg::X20, Reg::X19).word(), 0x3900_0274);
        assert_eq!(cmp_x_imm12(Reg::X0, 1).word(), 0xF100_041F);
        assert_eq!(b_ne(7).word(), 0x5400_00E1);
        assert_eq!(cbnz_x(Reg::X0, 15).word(), 0xB500_01E0);
        assert_eq!(tbnz_x(Reg::X0, 63, 13).word(), 0xB7F8_01A0);
    }

    #[test]
    fn serializes_words_as_little_endian_bytes() {
        let bytes = words_to_bytes::<2, 8>([Instruction::raw(0x1122_3344), B_LOOP]);
        assert_eq!(bytes, [0x44, 0x33, 0x22, 0x11, 0, 0, 0, 0x14]);
    }
}
