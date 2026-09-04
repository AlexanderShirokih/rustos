//! Ординал операции в const-контексте.
//!
//! Значение - младшие 64 бита SHA-256 канонического имени `<protocol>.<operation>`,
//! прочитанные big-endian. Ординал часть ABI: его считает `protocol!` во время компиляции,
//! и любое расхождение в этой реализации ломает совместимость с уже собранными сторонами.

#![allow(clippy::indexing_slicing)]

const H: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

const K: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

const fn compress(mut state: [u32; 8], block: &[u8; 64]) -> [u32; 8] {
    let mut w = [0u32; 64];

    let mut index = 0;
    while index < 16 {
        let base = index * 4;
        w[index] = ((block[base] as u32) << 24)
            | ((block[base + 1] as u32) << 16)
            | ((block[base + 2] as u32) << 8)
            | (block[base + 3] as u32);
        index += 1;
    }
    while index < 64 {
        let a = w[index - 15];
        let b = w[index - 2];
        let s0 = a.rotate_right(7) ^ a.rotate_right(18) ^ (a >> 3);
        let s1 = b.rotate_right(17) ^ b.rotate_right(19) ^ (b >> 10);
        w[index] = w[index - 16]
            .wrapping_add(s0)
            .wrapping_add(w[index - 7])
            .wrapping_add(s1);
        index += 1;
    }

    // Рабочие переменные раунда: индексы 0..7 - это a..h спецификации.
    let mut v = state;
    let mut round = 0;
    while round < 64 {
        let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
        let choose = (v[4] & v[5]) ^ ((!v[4]) & v[6]);
        let temp1 = v[7]
            .wrapping_add(s1)
            .wrapping_add(choose)
            .wrapping_add(K[round])
            .wrapping_add(w[round]);
        let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
        let majority = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
        let temp2 = s0.wrapping_add(majority);

        v[7] = v[6];
        v[6] = v[5];
        v[5] = v[4];
        v[4] = v[3].wrapping_add(temp1);
        v[3] = v[2];
        v[2] = v[1];
        v[1] = v[0];
        v[0] = temp1.wrapping_add(temp2);
        round += 1;
    }

    let mut index = 0;
    while index < 8 {
        state[index] = state[index].wrapping_add(v[index]);
        index += 1;
    }
    state
}

/// Младшие 64 бита SHA-256 от `name`, прочитанные big-endian.
#[must_use]
pub const fn ordinal_of(name: &str) -> u64 {
    let input = name.as_bytes();
    let mut state = H;
    let mut block = [0u8; 64];
    let mut filled = 0;

    let mut index = 0;
    while index < input.len() {
        block[filled] = input[index];
        filled += 1;
        index += 1;
        if filled == 64 {
            state = compress(state, &block);
            filled = 0;
        }
    }

    block[filled] = 0x80;
    filled += 1;
    if filled > 56 {
        while filled < 64 {
            block[filled] = 0;
            filled += 1;
        }
        state = compress(state, &block);
        filled = 0;
    }
    while filled < 56 {
        block[filled] = 0;
        filled += 1;
    }

    let bits = (input.len() as u64) * 8;
    let length = bits.to_be_bytes();
    let mut tail = 0;
    while tail < 8 {
        block[56 + tail] = length[tail];
        tail += 1;
    }
    state = compress(state, &block);

    // Младшие 64 бита 256-битного числа - это последние два слова состояния.
    ((state[6] as u64) << 32) | (state[7] as u64)
}

#[cfg(test)]
mod tests {
    use super::ordinal_of;

    #[test]
    fn matches_the_reference_digest() {
        assert_eq!(ordinal_of("abc"), 0xb410_ff61_f200_15ad);
        assert_eq!(ordinal_of(""), 0xa495_991b_7852_b855);
        assert_eq!(ordinal_of("Log.write"), 0xbc1b_2aa6_95a9_a3f4);
    }

    #[test]
    fn keeps_the_ordinals_of_the_declared_contracts() {
        // Ординал живущего контракта - часть ABI: значение закреплено, а не пересчитывается.
        assert_eq!(ordinal_of("Clock.now"), 0x6ca5_80d0_d09a_d48b);
        assert_eq!(ordinal_of("Clock.resolution"), 0xa846_34a0_4ab6_2111);
        assert_eq!(ordinal_of("Calc.add"), 0xb744_87cd_9b28_41ba);
        assert_eq!(ordinal_of("Calc.div"), 0x2420_7b95_a0d9_84fc);
        assert_eq!(ordinal_of("Calc.reset"), 0x1cb8_08a6_5161_cd40);
    }

    #[test]
    fn works_across_block_boundaries() {
        const CASES: [(usize, u64); 9] = [
            (54, 0x49ed_4146_8052_2952),
            (55, 0x9e09_89fa_c77e_4072),
            (56, 0x9afe_0a37_7c16_888e),
            (57, 0xb8f0_6db6_8cab_5217),
            (63, 0x4e9d_48d8_9776_d6f2),
            (64, 0x5fd6_2195_06b5_393c),
            (65, 0xb163_3ad5_65ae_d7e9),
            (119, 0x1b02_9e0d_9fa5_fb0c),
            (120, 0x36f3_7308_b276_ba98),
        ];
        const FILLER: &str = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";

        // Длины вокруг 55/56/64 байт разводят три ветки паддинга.
        for (len, expected) in CASES {
            assert_eq!(ordinal_of(&FILLER[..len]), expected, "длина {len}");
        }
    }
}
