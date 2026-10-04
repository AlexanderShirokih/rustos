//! E2E приватности состояния SIMD/FP: значения v0-v31 и FPCR/FPSR потока сохраняются через
//! переключение контекста и соседним потокам недоступны.
//!
//! Заливка регистров, переключения и вычитка идут одним asm-блоком. Компилятор своего
//! кода между ними не вставляет, и расхождение указывает на переключение контекста.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU32, Ordering};

use kernel_tests::kernel_test;
use runtime::{JoinHandle, Priority, Timeout, spawn_owned};
use syscall::SyscallOp;

const STACK_SIZE: u64 = 0x4000;
const JOIN_TIMEOUT: Timeout = Timeout::from_secs(10);
const WORKERS: usize = 6;
const MAIN_LANE: u64 = WORKERS as u64 + 1;
const ROUNDS: u32 = 256;

#[repr(C, align(16))]
#[derive(Clone, Copy, PartialEq, Eq)]
struct SimdState {
    vectors: [u128; 32],
    fpcr: u64,
    fpsr: u64,
}

const ZERO_STATE: SimdState = SimdState {
    vectors: [0; 32],
    fpcr: 0,
    fpsr: 0,
};

const _: () = assert!(
    core::mem::offset_of!(SimdState, fpcr) == 512,
    "asm addresses SimdState::fpcr by literal offset"
);
const _: () = assert!(
    core::mem::offset_of!(SimdState, fpsr) == 520,
    "asm addresses SimdState::fpsr by literal offset"
);

struct Shared {
    next_lane: AtomicU32,
    mismatches: AtomicU32,
}

fn pattern(lane: u64) -> SimdState {
    let mut state = SimdState {
        vectors: [0; 32],
        // Биты [26:22] FPCR - AHP, DN, FZ и режим округления. Трапы исключений FP
        // остаются выключенными: их биты [15:8] нулевые.
        fpcr: (lane & 0x1F) << 22,
        // Биты [4:0] FPSR - накопительные флаги исключений FP.
        fpsr: lane & 0x1F,
    };
    for (index, vector) in state.vectors.iter_mut().enumerate() {
        let low = lane
            .wrapping_mul(0x0101_0101_0101_0101)
            .wrapping_add(index as u64);
        *vector = (u128::from(!low) << 64) | u128::from(low);
    }
    state
}

fn cycle_simd_state(expected: &SimdState, observed: &mut SimdState, rounds: u32) {
    // SAFETY: указатели ведут на живые `SimdState` вызывающего, а обращения по ним лежат
    // в границах объекта: смещения фиксированы `repr(C)` и const-ассертами выше. `svc`
    // задаёт `ThreadYield`, аргументов у него нет и памяти ядру не передаётся. Регистры
    // v0-v31 и x0 объявлены изменяемыми блоком, и компилятор не держит в них живых
    // значений через него.
    unsafe {
        core::arch::asm!(
            "ldp q0,  q1,  [{expected}, #0]",
            "ldp q2,  q3,  [{expected}, #32]",
            "ldp q4,  q5,  [{expected}, #64]",
            "ldp q6,  q7,  [{expected}, #96]",
            "ldp q8,  q9,  [{expected}, #128]",
            "ldp q10, q11, [{expected}, #160]",
            "ldp q12, q13, [{expected}, #192]",
            "ldp q14, q15, [{expected}, #224]",
            "ldp q16, q17, [{expected}, #256]",
            "ldp q18, q19, [{expected}, #288]",
            "ldp q20, q21, [{expected}, #320]",
            "ldp q22, q23, [{expected}, #352]",
            "ldp q24, q25, [{expected}, #384]",
            "ldp q26, q27, [{expected}, #416]",
            "ldp q28, q29, [{expected}, #448]",
            "ldp q30, q31, [{expected}, #480]",
            "ldr {scratch}, [{expected}, #512]",
            "msr fpcr, {scratch}",
            "ldr {scratch}, [{expected}, #520]",
            "msr fpsr, {scratch}",
            "2:",
            "svc #{op}",
            "subs {rounds:w}, {rounds:w}, #1",
            "b.ne 2b",
            "stp q0,  q1,  [{observed}, #0]",
            "stp q2,  q3,  [{observed}, #32]",
            "stp q4,  q5,  [{observed}, #64]",
            "stp q6,  q7,  [{observed}, #96]",
            "stp q8,  q9,  [{observed}, #128]",
            "stp q10, q11, [{observed}, #160]",
            "stp q12, q13, [{observed}, #192]",
            "stp q14, q15, [{observed}, #224]",
            "stp q16, q17, [{observed}, #256]",
            "stp q18, q19, [{observed}, #288]",
            "stp q20, q21, [{observed}, #320]",
            "stp q22, q23, [{observed}, #352]",
            "stp q24, q25, [{observed}, #384]",
            "stp q26, q27, [{observed}, #416]",
            "stp q28, q29, [{observed}, #448]",
            "stp q30, q31, [{observed}, #480]",
            "mrs {scratch}, fpcr",
            "str {scratch}, [{observed}, #512]",
            "mrs {scratch}, fpsr",
            "str {scratch}, [{observed}, #520]",
            expected = in(reg) core::ptr::from_ref(expected),
            observed = in(reg) core::ptr::from_mut(observed),
            rounds = inout(reg) rounds => _,
            scratch = out(reg) _,
            op = const SyscallOp::ThreadYield as u16,
            out("x0") _,
            out("v0") _,
            out("v1") _,
            out("v2") _,
            out("v3") _,
            out("v4") _,
            out("v5") _,
            out("v6") _,
            out("v7") _,
            out("v8") _,
            out("v9") _,
            out("v10") _,
            out("v11") _,
            out("v12") _,
            out("v13") _,
            out("v14") _,
            out("v15") _,
            out("v16") _,
            out("v17") _,
            out("v18") _,
            out("v19") _,
            out("v20") _,
            out("v21") _,
            out("v22") _,
            out("v23") _,
            out("v24") _,
            out("v25") _,
            out("v26") _,
            out("v27") _,
            out("v28") _,
            out("v29") _,
            out("v30") _,
            out("v31") _,
        );
    }
}

fn spawn_noise_worker(shared: &Arc<Shared>) -> JoinHandle {
    let shared = Arc::clone(shared);
    spawn_owned(move || simd_worker(&shared), STACK_SIZE, Priority::new(1)).expect("spawn worker")
}

fn simd_worker(shared: &Shared) -> u32 {
    let lane = u64::from(shared.next_lane.fetch_add(1, Ordering::Relaxed));
    let expected = pattern(lane);
    let mut observed = ZERO_STATE;
    cycle_simd_state(&expected, &mut observed, ROUNDS);

    if observed != expected {
        shared.mismatches.fetch_add(1, Ordering::Relaxed);
    }
    0
}

#[kernel_test]
fn simd_state_is_private_to_the_thread() {
    let shared = Arc::new(Shared {
        next_lane: AtomicU32::new(1),
        mismatches: AtomicU32::new(0),
    });

    let threads: [_; WORKERS] = core::array::from_fn(|_| spawn_noise_worker(&shared));

    let expected = pattern(MAIN_LANE);
    let mut observed = ZERO_STATE;
    cycle_simd_state(&expected, &mut observed, ROUNDS);
    let main_keeps_state = observed == expected;

    for thread in threads {
        thread
            .join(JOIN_TIMEOUT)
            .expect("worker joins before timeout");
    }

    kernel_tests::kassert!(main_keeps_state);
    kernel_tests::kassert_eq!(shared.mismatches.load(Ordering::Relaxed), 0);
}
