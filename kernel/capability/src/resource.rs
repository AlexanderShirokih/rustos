//! `Resource` capability target: capability-токен на минтинг `Memory`-регионов из оперативной
//! памяти и носитель ресурсного бюджета (метеринг).
//!
//! Объект хранит:
//! - потолок доступа, который [`split`](Resource::split) копирует в производный ресурс;
//! - атомарный `budget` (в страницах/фреймах): метеринг операций, расходующих физпамять;
//! - родителя, из бюджета которого выделен собственный.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use memory::{AccessMask, BudgetRefund};
use syscall::MAX_RESOURCE_DEPTH;

use super::errors::IpcError;

#[derive(Debug)]
pub struct Resource {
    access_mask: AccessMask,
    budget: AtomicU64,
    parent: Option<Arc<Resource>>,
    depth: u16,
}

impl Resource {
    /// Создаёт ресурс с потолком доступа `access_mask` и начальным бюджетом `budget_pages`
    /// страниц. Ресурс корневой, его бюджет ни с кого не списывается, и глубина деривации под
    /// ним начинается с нуля.
    pub fn new(access_mask: AccessMask, budget_pages: u64) -> Arc<Self> {
        Arc::new(Self {
            access_mask,
            budget: AtomicU64::new(budget_pages),
            parent: None,
            depth: 0,
        })
    }

    /// Выделяет `budget_pages` страниц бюджета в отдельный ресурс с тем же
    /// потолком доступа. Страницы списываются с текущего ресурса
    /// и возвращаются ему, когда дочерний уходит. При нехватке бюджета либо на
    /// глубине [`MAX_RESOURCE_DEPTH`] возвращает `ResourceExhausted`.
    pub fn split(self: &Arc<Self>, budget_pages: u64) -> Result<Arc<Self>, IpcError> {
        if self.depth >= MAX_RESOURCE_DEPTH {
            return Err(IpcError::ResourceExhausted);
        }
        self.try_consume(budget_pages)?;
        Ok(Arc::new(Self {
            access_mask: self.access_mask,
            budget: AtomicU64::new(budget_pages),
            parent: Some(Arc::clone(self)),
            depth: self.depth + 1,
        }))
    }

    /// Ресурс, из бюджета которого выделен собственный. `None` у корневого.
    pub fn parent(&self) -> Option<&Arc<Resource>> {
        self.parent.as_ref()
    }

    pub fn access_mask(&self) -> AccessMask {
        self.access_mask
    }

    pub fn remaining_budget(&self) -> u64 {
        self.budget.load(Ordering::Acquire)
    }

    /// Атомарно списывает `amount` страниц с бюджета, если их хватает. На нехватке -
    /// [`IpcError::ResourceExhausted`], и бюджет остаётся прежним. Бюджет родителя не трогается.
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

    /// Операция, парная к [`try_consume`](Self::try_consume), возвращает `amount` страниц в
    /// бюджет. Начальный бюджет потолком не служит, и сложение насыщается на `u64::MAX`. Парность
    /// вызовов остаётся за вызывающим.
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
}

impl Drop for Resource {
    fn drop(&mut self) {
        // Раскрутка цепочки рекурсивна. Её глубину ограничивает `split`.
        if let Some(parent) = &self.parent {
            parent.release(*self.budget.get_mut());
        }
    }
}

/// [`BudgetRefund`](BudgetRefund), возвращающий `pages` бюджету.
pub struct ResourceBudgetRefund {
    resource: Arc<Resource>,
    pages: u64,
}

impl ResourceBudgetRefund {
    pub fn new(resource: &Arc<Resource>, pages: u64) -> Arc<Self> {
        Arc::new(Self {
            // Ссылка сильная. Ресурс остаётся в памяти дольше выданных из него регионов и
            // отдаёт родителю весь свой бюджет, включая неизрасходованный остаток.
            resource: Arc::clone(resource),
            pages,
        })
    }
}

impl BudgetRefund for ResourceBudgetRefund {
    fn refund(&self) {
        self.resource.release(self.pages);
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroUsize;

    use memory::{MemoryRegion, physical_address::PageAlignedAddress};

    use super::*;

    fn pa(raw: usize) -> PageAlignedAddress {
        PageAlignedAddress::from_usize(raw).unwrap()
    }

    fn nz(raw: usize) -> NonZeroUsize {
        NonZeroUsize::new(raw).unwrap()
    }

    #[test]
    fn try_consume_success_decrements_budget() {
        let resource = Resource::new(AccessMask::RW, 10);

        assert_eq!(resource.try_consume(4), Ok(()));
        assert_eq!(resource.remaining_budget(), 6);
    }

    #[test]
    fn try_consume_exact_exhaustion_succeeds() {
        let resource = Resource::new(AccessMask::RW, 8);

        assert_eq!(resource.try_consume(8), Ok(()));
        assert_eq!(resource.remaining_budget(), 0);
    }

    #[test]
    fn try_consume_over_budget_rejected_and_budget_unchanged() {
        let resource = Resource::new(AccessMask::RW, 3);

        assert_eq!(resource.try_consume(4), Err(IpcError::ResourceExhausted));
        assert_eq!(resource.remaining_budget(), 3);
    }

    #[test]
    fn try_consume_zero_is_always_ok() {
        let resource = Resource::new(AccessMask::RW, 0);

        assert_eq!(resource.try_consume(0), Ok(()));
        assert_eq!(resource.remaining_budget(), 0);
    }

    #[test]
    fn release_returns_pages_to_budget() {
        let resource = Resource::new(AccessMask::RW, 10);
        resource.try_consume(4).unwrap();
        assert_eq!(resource.remaining_budget(), 6);

        resource.release(4);
        assert_eq!(resource.remaining_budget(), 10);
    }

    #[test]
    fn consume_then_release_round_trips() {
        let resource = Resource::new(AccessMask::RW, 8);
        for _ in 0..100 {
            resource.try_consume(3).unwrap();
            resource.release(3);
        }
        assert_eq!(resource.remaining_budget(), 8);
    }

    #[test]
    fn release_saturates_instead_of_overflowing() {
        let resource = Resource::new(AccessMask::RW, u64::MAX - 1);
        resource.release(10);
        assert_eq!(resource.remaining_budget(), u64::MAX);
    }

    // дропе региона.
    #[test]
    fn dropping_minted_physical_region_refunds_budget() {
        let resource = Resource::new(AccessMask::RW, 10);

        resource.try_consume(4).unwrap();
        let refund = ResourceBudgetRefund::new(&resource, 4);
        let region =
            MemoryRegion::create_physical_device(pa(0x4000_0000), nz(0x4000), AccessMask::RW)
                .with_refund(refund);
        // Пока регион жив - бюджет остаётся списанным.
        assert_eq!(resource.remaining_budget(), 6);

        drop(region);
        // Регион закрыт - бюджет вернулся ресурсу.
        assert_eq!(resource.remaining_budget(), 10);
    }

    #[test]
    fn many_mint_close_cycles_do_not_leak_budget() {
        let resource = Resource::new(AccessMask::RW, 4);

        // 4 страницы бюджета, минтим/закрываем регион на 4 страницы 1000 раз.
        // Без refund второй цикл уже упёрся бы в ResourceExhausted.
        for _ in 0..1000 {
            resource.try_consume(4).unwrap();
            let refund = ResourceBudgetRefund::new(&resource, 4);
            let region =
                MemoryRegion::create_physical_device(pa(0x4000_0000), nz(0x4000), AccessMask::RW)
                    .with_refund(refund);
            drop(region);
        }
        assert_eq!(resource.remaining_budget(), 4);
    }

    #[test]
    fn split_moves_pages_from_parent_to_child() {
        let parent = Resource::new(AccessMask::RW, 10);

        let child = parent.split(4).expect("budget must suffice");

        assert_eq!(parent.remaining_budget(), 6);
        assert_eq!(child.remaining_budget(), 4);
    }

    #[test]
    fn split_keeps_access_ceiling_of_parent() {
        let parent = Resource::new(AccessMask::R, 10);

        let child = parent.split(4).expect("budget must suffice");

        assert_eq!(child.access_mask().bits(), parent.access_mask().bits());
    }

    #[test]
    fn split_over_parent_budget_is_rejected_and_budget_unchanged() {
        let parent = Resource::new(AccessMask::RW, 3);

        assert!(matches!(
            parent.split(4).err(),
            Some(IpcError::ResourceExhausted)
        ));
        assert_eq!(parent.remaining_budget(), 3);
    }

    #[test]
    fn dropping_child_returns_unspent_budget_to_parent() {
        let parent = Resource::new(AccessMask::RW, 10);
        let child = parent.split(6).expect("budget must suffice");
        child.try_consume(2).unwrap();
        assert_eq!(parent.remaining_budget(), 4);

        drop(child);
        // Возвращается остаток. Списанные страницы возвращает регион, который
        // их держит, и до тех пор он не даёт ресурсу уйти.
        assert_eq!(parent.remaining_budget(), 8);
    }

    #[test]
    fn region_minted_from_child_keeps_it_alive_until_closed() {
        let parent = Resource::new(AccessMask::RW, 10);
        let child = parent.split(4).expect("budget must suffice");
        child.try_consume(4).unwrap();
        let region =
            MemoryRegion::create_physical_device(pa(0x4000_0000), nz(0x4000), AccessMask::RW)
                .with_refund(ResourceBudgetRefund::new(&child, 4));

        drop(child);
        // Регион держит дочерний ресурс. Его страницы родителю ещё не
        // вернулись.
        assert_eq!(parent.remaining_budget(), 6);

        drop(region);
        assert_eq!(parent.remaining_budget(), 10);
    }

    #[test]
    fn split_depth_is_capped() {
        // Закрытая капа не убивает объект. Его держит поле `parent` потомка, а потолок
        // отбирает у userspace длину цепочки в куче ядра.
        let root = Resource::new(AccessMask::RW, u64::MAX);
        let mut leaf = Arc::clone(&root);
        for _ in 0..MAX_RESOURCE_DEPTH {
            leaf = leaf.split(1).expect("split within the depth ceiling");
        }

        assert!(matches!(
            leaf.split(1).err(),
            Some(IpcError::ResourceExhausted)
        ));
        // Отказ по глубине не тронул бюджет листа. Лист сам расходует свой бюджет.
        assert_eq!(leaf.remaining_budget(), 1);
    }

    #[test]
    fn split_chain_at_the_depth_ceiling_unwinds_budget() {
        let root = Resource::new(AccessMask::RW, 1);
        let mut leaf = Arc::clone(&root);
        for _ in 0..MAX_RESOURCE_DEPTH {
            leaf = leaf.split(1).expect("each level hands its page down");
        }
        assert_eq!(root.remaining_budget(), 0);

        drop(leaf);
        assert_eq!(root.remaining_budget(), 1);
    }

    #[test]
    fn split_chain_returns_budget_through_every_level() {
        let root = Resource::new(AccessMask::RW, 10);
        let middle = root.split(8).expect("budget must suffice");
        let leaf = middle.split(5).expect("budget must suffice");
        assert_eq!(root.remaining_budget(), 2);

        drop(leaf);
        assert_eq!(middle.remaining_budget(), 8);

        drop(middle);
        assert_eq!(root.remaining_budget(), 10);
    }
}
