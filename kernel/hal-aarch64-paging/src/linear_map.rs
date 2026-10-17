//! Неизменяемый снимок линейной карты физической памяти ядра.

use memory::{
    physical_address::PhysicalAddress,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};

use crate::{
    mapper::{PageMapper, extract_table_pa},
    mem_flags::{Aarch64MemFlags, Access},
    memory_attribute::MemoryAttribute,
    table_alloc::TableAlloc,
    user_copy::UserAccess,
};

/// Предельное число диапазонов физической памяти в [`LinearMap`].
pub const LINEAR_RANGES_MAX: usize = 128;

#[derive(Clone, Copy)]
struct PhysicalRange {
    start: usize,
    end: usize,
    writable: bool,
}

/// Снимок кэшируемых диапазонов физической памяти, доступных только EL1 через линейную карту
/// ядра. Ядро обращается к физическому адресу `pa` по виртуальному адресу `pa + base`.
pub struct LinearMap {
    base: PageAlignedVirtualAddress,
    ranges: [PhysicalRange; LINEAR_RANGES_MAX],
    len: usize,
}

/// Отказ [`LinearMap::capture`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinearMapError {
    /// Окно выходит за конец адресного пространства.
    WindowOverflow,
    /// Снимок требует больше [`LINEAR_RANGES_MAX`] диапазонов после объединения соседних
    /// диапазонов с одинаковыми правами.
    TooManyRanges,
}

impl LinearMap {
    /// Создаёт снимок отображений `kernel` в окне `[base, base + size)`.
    /// Снимок включает кэшируемые leaf-записи, которые доступны только EL1 и отображают
    /// физический адрес `pa` на виртуальный адрес `pa + base`.
    pub fn capture<A: TableAlloc>(
        kernel: &PageMapper<A>,
        base: PageAlignedVirtualAddress,
        size: usize,
    ) -> Result<Self, LinearMapError> {
        let start = base.as_usize();
        let end = start
            .checked_add(size)
            .ok_or(LinearMapError::WindowOverflow)?;
        let mut map = Self {
            base,
            ranges: [PhysicalRange {
                start: 0,
                end: 0,
                writable: false,
            }; LINEAR_RANGES_MAX],
            len: 0,
        };

        if size == 0 {
            return Ok(map);
        }
        let mut error = None;
        kernel.visit_leaves(base.as_virtual(), &mut |va, shift, raw| {
            if error.is_some() {
                return;
            }
            let va = va.as_usize();
            let span = 1usize << shift;
            let range_start = va.max(start);
            let range_end = va.saturating_add(span).min(end);
            if range_start >= range_end {
                return;
            }
            let pa = extract_table_pa(raw).as_usize() & !(span - 1);
            if pa + (range_start - va) == range_start - start
                && let Some(writable) = kernel_access(Aarch64MemFlags::from_bits(raw))
            {
                error = map
                    .push(PhysicalRange {
                        start: range_start - start,
                        end: range_end - start,
                        writable,
                    })
                    .err();
            }
        });
        error.map_or(Ok(map), Err)
    }

    /// Возвращает адрес линейной карты для `pa`, если снимок разрешает EL1 доступ `access`.
    /// Если границы окна снимка выровнены по странице, разрешение действует на всю
    /// 4К-страницу, содержащую `pa`.
    pub fn alias(&self, pa: PhysicalAddress, access: UserAccess) -> Option<VirtualAddress> {
        let pa = pa.as_usize();
        let ranges = self.ranges.get(..self.len)?;
        let range = ranges.get(ranges.partition_point(|range| range.end <= pa))?;
        let permitted = range.start <= pa && (range.writable || access == UserAccess::Read);
        permitted
            .then(|| pa.checked_add(self.base.as_usize()))
            .flatten()
            .map(VirtualAddress::new)
    }

    fn push(&mut self, range: PhysicalRange) -> Result<(), LinearMapError> {
        if let Some(last) = self
            .len
            .checked_sub(1)
            .and_then(|index| self.ranges.get_mut(index))
            && last.end == range.start
            && last.writable == range.writable
        {
            last.end = range.end;
            return Ok(());
        }
        let slot = self
            .ranges
            .get_mut(self.len)
            .ok_or(LinearMapError::TooManyRanges)?;
        *slot = range;
        self.len += 1;
        Ok(())
    }
}

fn kernel_access(flags: Aarch64MemFlags) -> Option<bool> {
    if flags.get_attr_index() != MemoryAttribute::Cached.index() {
        return None;
    }
    match flags.get_ap() {
        Access::KernelRW => Some(true),
        Access::KernelRO => Some(false),
        Access::UserRW | Access::UserRO => None,
    }
}
