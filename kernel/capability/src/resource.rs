//! `Resource` capability target: capability-токен на минтинг `Memory`-регионов поверх
//! фиксированного физического диапазона (MMIO, DMA-буферы) и носитель
//! ресурсного бюджета (метеринг).
//!
//! Объект хранит:
//! - границы и потолок доступа: право [`Rights::WRITE`](super::Rights::WRITE) разрешает минтить
//!   только поддиапазоны этого ресурса;
//! - атомарный `budget` (в страницах/фреймах): метеринг операций, расходующих физпамять.

use alloc::sync::{Arc, Weak};
use core::{
    num::NonZeroUsize,
    sync::atomic::{AtomicU64, Ordering},
};

use memory::{AccessMask, BudgetRefund, physical_address::PageAlignedAddress};

use super::errors::IpcError;

#[derive(Debug)]
pub struct Resource {
    pa_base: PageAlignedAddress,
    size_bytes: NonZeroUsize,
    access_mask: AccessMask,
    budget: AtomicU64,
}

impl Resource {
    /// Создаёт ресурс с диапазоном `[pa_base, pa_base + size_bytes)`, потолком
    /// доступа `access_mask` и начальным бюджетом `budget_pages` страниц.
    pub fn new(
        pa_base: PageAlignedAddress,
        size_bytes: NonZeroUsize,
        access_mask: AccessMask,
        budget_pages: u64,
    ) -> Arc<Self> {
        Arc::new(Self {
            pa_base,
            size_bytes,
            access_mask,
            budget: AtomicU64::new(budget_pages),
        })
    }

    pub fn pa_base(&self) -> PageAlignedAddress {
        self.pa_base
    }

    pub fn size_bytes(&self) -> NonZeroUsize {
        self.size_bytes
    }

    pub fn access_mask(&self) -> AccessMask {
        self.access_mask
    }

    pub fn remaining_budget(&self) -> u64 {
        self.budget.load(Ordering::Acquire)
    }

    /// Атомарно списывает `amount` страниц с бюджета, если их хватает.
    pub fn try_consume(&self, amount: u64) -> Result<(), IpcError> {
        let mut current = self.budget.load(Ordering::Acquire);
        loop {
            if current < amount {
                return Err(IpcError::ResourceExhausted);
            }
            let next = current - amount;
            match self.budget.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(observed) => current = observed,
            }
        }
    }

    /// Парная к [`try_consume`](Self::try_consume): возвращает `amount` страниц в бюджет.
    pub fn release(&self, amount: u64) {
        let mut current = self.budget.load(Ordering::Acquire);
        loop {
            let next = current.saturating_add(amount);
            match self.budget.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(observed) => current = observed,
            }
        }
    }

    pub fn permits(
        &self,
        pa_base: PageAlignedAddress,
        size_bytes: NonZeroUsize,
        access_mask: AccessMask,
    ) -> bool {
        if !self.access_mask.allows(access_mask) {
            return false;
        }

        let resource_start = self.pa_base.as_usize();
        let Some(resource_end) = resource_start.checked_add(self.size_bytes.get()) else {
            return false;
        };
        let requested_start = pa_base.as_usize();
        let Some(requested_end) = requested_start.checked_add(size_bytes.get()) else {
            return false;
        };

        resource_start <= requested_start && requested_end <= resource_end
    }
}

/// [`BudgetRefund`](BudgetRefund), возвращающий `pages` бюджету.
pub struct ResourceBudgetRefund {
    resource: Weak<Resource>,
    pages: u64,
}

impl ResourceBudgetRefund {
    pub fn new(resource: &Arc<Resource>, pages: u64) -> Arc<Self> {
        Arc::new(Self {
            resource: Arc::downgrade(resource),
            pages,
        })
    }
}

impl BudgetRefund for ResourceBudgetRefund {
    fn refund(&self) {
        if let Some(resource) = self.resource.upgrade() {
            resource.release(self.pages);
        }
    }
}

#[cfg(test)]
mod tests {
    use memory::MemoryRegion;

    use super::*;

    fn pa(raw: usize) -> PageAlignedAddress {
        PageAlignedAddress::from_usize(raw).unwrap()
    }

    fn nz(raw: usize) -> NonZeroUsize {
        NonZeroUsize::new(raw).unwrap()
    }

    #[test]
    fn permits_subrange_with_subset_access() {
        let resource = Resource::new(pa(0x4000_0000), nz(0x4000), AccessMask::RW, 0);

        assert!(resource.permits(pa(0x4000_1000), nz(0x1000), AccessMask::R));
    }

    #[test]
    fn rejects_range_outside_resource() {
        let resource = Resource::new(pa(0x4000_0000), nz(0x4000), AccessMask::RW, 0);

        assert!(!resource.permits(pa(0x4000_3000), nz(0x2000), AccessMask::R));
        assert!(!resource.permits(pa(0x3fff_f000), nz(0x2000), AccessMask::R));
    }

    #[test]
    fn rejects_access_outside_resource_mask() {
        let resource = Resource::new(pa(0x4000_0000), nz(0x4000), AccessMask::R, 0);

        assert!(!resource.permits(pa(0x4000_0000), nz(0x1000), AccessMask::RW));
    }

    #[test]
    fn rejects_overflowing_requested_range() {
        let resource = Resource::new(pa(0xffff_f000), nz(0x1000), AccessMask::R, 0);

        assert!(!resource.permits(pa(0xffff_f000), nz(usize::MAX), AccessMask::R));
    }

    #[test]
    fn try_consume_success_decrements_budget() {
        let resource = Resource::new(pa(0x4000_0000), nz(0x1000), AccessMask::RW, 10);

        assert_eq!(resource.try_consume(4), Ok(()));
        assert_eq!(resource.remaining_budget(), 6);
    }

    #[test]
    fn try_consume_exact_exhaustion_succeeds() {
        let resource = Resource::new(pa(0x4000_0000), nz(0x1000), AccessMask::RW, 8);

        assert_eq!(resource.try_consume(8), Ok(()));
        assert_eq!(resource.remaining_budget(), 0);
    }

    #[test]
    fn try_consume_over_budget_rejected_and_budget_unchanged() {
        let resource = Resource::new(pa(0x4000_0000), nz(0x1000), AccessMask::RW, 3);

        assert_eq!(resource.try_consume(4), Err(IpcError::ResourceExhausted));
        assert_eq!(resource.remaining_budget(), 3);
    }

    #[test]
    fn try_consume_zero_is_always_ok() {
        let resource = Resource::new(pa(0x4000_0000), nz(0x1000), AccessMask::RW, 0);

        assert_eq!(resource.try_consume(0), Ok(()));
        assert_eq!(resource.remaining_budget(), 0);
    }

    #[test]
    fn release_returns_pages_to_budget() {
        let resource = Resource::new(pa(0x4000_0000), nz(0x1000), AccessMask::RW, 10);
        resource.try_consume(4).unwrap();
        assert_eq!(resource.remaining_budget(), 6);

        resource.release(4);
        assert_eq!(resource.remaining_budget(), 10);
    }

    #[test]
    fn consume_then_release_round_trips() {
        let resource = Resource::new(pa(0x4000_0000), nz(0x1000), AccessMask::RW, 8);
        for _ in 0..100 {
            resource.try_consume(3).unwrap();
            resource.release(3);
        }
        assert_eq!(resource.remaining_budget(), 8);
    }

    #[test]
    fn release_saturates_instead_of_overflowing() {
        let resource = Resource::new(pa(0x4000_0000), nz(0x1000), AccessMask::RW, u64::MAX - 1);
        resource.release(10);
        assert_eq!(resource.remaining_budget(), u64::MAX);
    }

    // дропе региона.
    #[test]
    fn dropping_minted_physical_region_refunds_budget() {
        let resource = Resource::new(pa(0x4000_0000), nz(0x4000), AccessMask::RW, 10);

        resource.try_consume(4).unwrap();
        let refund = ResourceBudgetRefund::new(&resource, 4);
        let region = MemoryRegion::create_physical(pa(0x4000_0000), nz(0x4000), AccessMask::RW)
            .with_refund(refund);
        // Пока регион жив - бюджет остаётся списанным.
        assert_eq!(resource.remaining_budget(), 6);

        drop(region);
        // Регион закрыт - бюджет вернулся ресурсу.
        assert_eq!(resource.remaining_budget(), 10);
    }

    #[test]
    fn many_mint_close_cycles_do_not_leak_budget() {
        let resource = Resource::new(pa(0x4000_0000), nz(0x4000), AccessMask::RW, 4);

        // 4 страницы бюджета, минтим/закрываем регион на 4 страницы 1000 раз.
        // Без refund второй цикл уже упёрся бы в ResourceExhausted.
        for _ in 0..1000 {
            resource.try_consume(4).unwrap();
            let refund = ResourceBudgetRefund::new(&resource, 4);
            let region = MemoryRegion::create_physical(pa(0x4000_0000), nz(0x4000), AccessMask::RW)
                .with_refund(refund);
            drop(region);
        }
        assert_eq!(resource.remaining_budget(), 4);
    }

    #[test]
    fn refund_after_resource_destroyed_is_noop() {
        let refund: Arc<ResourceBudgetRefund>;

        {
            let resource = Resource::new(pa(0x4000_0000), nz(0x1000), AccessMask::RW, 4);
            resource.try_consume(4).unwrap();
            refund = ResourceBudgetRefund::new(&resource, 4);
        }

        let region = MemoryRegion::create_physical(pa(0x4000_0000), nz(0x1000), AccessMask::RW)
            .with_refund(refund);

        drop(region); // не должно паниковать
    }
}
