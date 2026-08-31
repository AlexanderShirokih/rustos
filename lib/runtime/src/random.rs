//! Псевдослучайные байты для рандомизации хеш-таблиц.
//!
//! Источника энтропии в системе нет. Генератор — SplitMix64, засеянный монотонными часами:
//! поток меняется от запуска к запуску, но восстанавливается по времени старта процесса.
//!
//! Секретности он не даёт и системным источником случайности не является. `SystemRng`
//! стандартной библиотеки на него не выведен: там контракт требует пригодности для
//! криптографии, и вместо тихой подмены запрос падает. Ничему, что зависит от
//! непредсказуемости, доверять этот поток нельзя.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::clock::Instant;

const GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

const MIX_A: u64 = 0xBF58_476D_1CE4_E5B9;

const MIX_B: u64 = 0x94D0_49BB_1331_11EB;

static STATE: AtomicU64 = AtomicU64::new(0);

/// Заполняет буфер псевдослучайными байтами.
pub fn fill_bytes(bytes: &mut [u8]) {
    seed_once();
    for chunk in bytes.chunks_mut(size_of::<u64>()) {
        let word = next().to_ne_bytes();
        chunk.copy_from_slice(&word[..chunk.len()]);
    }
}

fn seed_once() {
    if STATE.load(Ordering::Relaxed) != 0 {
        return;
    }
    // Ноль означает «не засеяно», поэтому младший бит поднят: часы могут показать ровно ноль.
    let seed = Instant::now().as_nanos() | 1;
    let _ = STATE.compare_exchange(0, seed, Ordering::Relaxed, Ordering::Relaxed);
}

fn next() -> u64 {
    let mut word = STATE
        .fetch_add(GAMMA, Ordering::Relaxed)
        .wrapping_add(GAMMA);
    word = (word ^ (word >> 30)).wrapping_mul(MIX_A);
    word = (word ^ (word >> 27)).wrapping_mul(MIX_B);
    word ^ (word >> 31)
}
