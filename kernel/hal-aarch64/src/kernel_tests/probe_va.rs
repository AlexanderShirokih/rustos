//! VA-полосы, которые kernel-тесты занимают в своих user-AS.
//!
//! Полоса принадлежит одному тесту. Активный AS одного теста не транслирует
//! адреса другого, и TLB-запись, сохранившаяся через переключение, остаётся его
//! собственной. Непересечение полос удерживает `assert` ниже.

use hal_aarch64_paging::level::{L1, L2, Level};

const MIB: usize = 1 << 20;
const L2_BLOCK_SIZE: usize = 1usize << L2::SHIFT;
const L1_BLOCK_SIZE: usize = 1usize << L1::SHIFT;

pub(super) struct VaWindow {
    base: usize,
    len: usize,
}

impl VaWindow {
    const fn new(base: usize, len: usize) -> Self {
        Self { base, len }
    }

    pub(super) const fn base(&self) -> usize {
        self.base
    }

    const fn end(&self) -> usize {
        self.base + self.len
    }

    const fn is_disjoint(&self, other: &Self) -> bool {
        self.end() <= other.base || other.end() <= self.base
    }
}

pub(super) const PAGE_FLAGS: VaWindow = VaWindow::new(0x5200_0000, MIB);

pub(super) const ASID: VaWindow = VaWindow::new(0x5400_0000, MIB);

pub(super) const UNMAP: VaWindow = VaWindow::new(0x6000_0000, 8 * MIB);

pub(super) const UNMAP_L2_BLOCK: VaWindow = VaWindow::new(0x7000_0000, L2_BLOCK_SIZE);

pub(super) const UNMAP_L1_BLOCK: VaWindow = VaWindow::new(0x8000_0000, L1_BLOCK_SIZE);

pub(super) const UNMAP_PARTIAL_BLOCK: VaWindow = VaWindow::new(0xC000_0000, L2_BLOCK_SIZE);

const WINDOWS: [&VaWindow; 6] = [
    &PAGE_FLAGS,
    &ASID,
    &UNMAP,
    &UNMAP_L2_BLOCK,
    &UNMAP_L1_BLOCK,
    &UNMAP_PARTIAL_BLOCK,
];

const _: () = {
    let mut window = 0;
    while window < WINDOWS.len() {
        let mut other = window + 1;
        while other < WINDOWS.len() {
            assert!(
                WINDOWS[window].is_disjoint(WINDOWS[other]),
                "probe windows of user-AS kernel tests must stay disjoint"
            );
            other += 1;
        }
        window += 1;
    }
};
