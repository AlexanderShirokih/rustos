//! Граф деривации capability и каскадный отзыв.

use core::sync::atomic::{AtomicBool, Ordering};

use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};

use collections::{LockCell, MutexCell};

/// Обработчик снятие побочного эффекта капы, вызываемый каскадом при отзыве.
pub trait RevocationHook: Send + Sync {
    fn revoke(&self);
}

/// Узел графа деривации.
pub struct RevNode {
    parent: Option<Weak<RevNode>>,
    children: MutexCell<Vec<Weak<RevNode>>>,
    hooks: MutexCell<Vec<Weak<dyn RevocationHook>>>,
    revoked: AtomicBool,
}

impl RevNode {
    /// Создаёт корневой узел.
    pub fn new_root() -> Arc<Self> {
        Arc::new(Self {
            parent: None,
            children: MutexCell::new(Vec::new()),
            hooks: MutexCell::new(Vec::new()),
            revoked: AtomicBool::new(false),
        })
    }

    /// Дочерний узел, производный от `parent`.
    /// Если `parent` уже отозван (`revoked` установлен), возвращает `None`.
    pub fn new_child(parent: &Arc<RevNode>) -> Option<Arc<Self>> {
        let child = Arc::new(Self {
            parent: Some(Arc::downgrade(parent)),
            children: MutexCell::new(Vec::new()),
            hooks: MutexCell::new(Vec::new()),
            revoked: AtomicBool::new(false),
        });
        let inserted = parent.children.with_lock(|c| {
            if parent.revoked.load(Ordering::SeqCst) {
                return false;
            }
            c.retain(|w| w.strong_count() > 0);
            c.push(Arc::downgrade(&child));
            true
        });
        if inserted { Some(child) } else { None }
    }

    /// Регистрирует `Weak` хука отзыва побочного эффекта.
    /// Если узел уже отозван, хук вызывается немедленно.
    pub fn register_hook(&self, hook: Weak<dyn RevocationHook>) {
        self.hooks.with_lock(|h| {
            if self.revoked.load(Ordering::SeqCst) {
                if let Some(live) = hook.upgrade() {
                    live.revoke();
                }
                return;
            }
            h.retain(|w| w.strong_count() > 0);
            h.push(hook);
        });
    }

    /// Валидна, если жив каждый предок по цепочке.
    pub fn is_alive(self: &Arc<Self>) -> bool {
        let mut current = self.clone();
        loop {
            let Some(weak_parent) = &current.parent else {
                // Дошли до корня - все предки живы.
                return true;
            };
            let Some(parent) = weak_parent.upgrade() else {
                // Предок закрыт - капа отозвана.
                return false;
            };
            current = parent;
        }
    }
}

/// Каскадный отзыв поддерева: вызывает хуки снятия побочных эффектов по всему поддереву.
pub fn revoke_subtree(node: &Arc<RevNode>) {
    let mut stack: Vec<Arc<RevNode>> = Vec::new();
    stack.push(node.clone());
    while let Some(current) = stack.pop() {
        current.revoked.store(true, Ordering::SeqCst);

        let hooks: Vec<Weak<dyn RevocationHook>> = current.hooks.with_lock(|h| h.clone());
        for weak in hooks {
            if let Some(hook) = weak.upgrade() {
                hook.revoke();
            }
        }

        let children: Vec<Weak<RevNode>> = current.children.with_lock(|c| c.clone());
        for weak in children {
            if let Some(child) = weak.upgrade() {
                stack.push(child);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct CountingHook(AtomicUsize);

    impl CountingHook {
        fn new() -> Arc<Self> {
            Arc::new(Self(AtomicUsize::new(0)))
        }

        fn count(&self) -> usize {
            self.0.load(Ordering::Acquire)
        }
    }

    impl RevocationHook for CountingHook {
        fn revoke(&self) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[test]
    fn root_is_alive() {
        let root = RevNode::new_root();
        assert!(root.is_alive());
    }

    #[test]
    fn child_alive_while_parent_alive() {
        let root = RevNode::new_root();
        let child = RevNode::new_child(&root).unwrap();
        assert!(child.is_alive());
    }

    #[test]
    fn child_dies_when_parent_dropped() {
        let root = RevNode::new_root();
        let child = RevNode::new_child(&root).unwrap();
        drop(root);
        assert!(!child.is_alive());
    }

    #[test]
    fn grandchild_dies_through_dead_ancestor() {
        let root = RevNode::new_root();
        let mid = RevNode::new_child(&root).unwrap();
        let leaf = RevNode::new_child(&mid).unwrap();
        assert!(leaf.is_alive());
        drop(root);
        assert!(!leaf.is_alive());
        assert!(!mid.is_alive());
    }

    #[test]
    fn siblings_are_independent() {
        let root = RevNode::new_root();
        let a = RevNode::new_child(&root).unwrap();
        let b = RevNode::new_child(&root).unwrap();
        drop(a);

        assert!(b.is_alive());
    }

    #[test]
    fn revoke_subtree_fires_node_and_descendant_hooks_only() {
        let root = RevNode::new_root();
        let child = RevNode::new_child(&root).unwrap();
        let grandchild = RevNode::new_child(&child).unwrap();
        let sibling = RevNode::new_child(&root).unwrap();

        let h_child = CountingHook::new();
        let h_grandchild = CountingHook::new();
        let h_sibling = CountingHook::new();
        child.register_hook(Arc::downgrade(&h_child) as Weak<dyn RevocationHook>);
        grandchild.register_hook(Arc::downgrade(&h_grandchild) as Weak<dyn RevocationHook>);
        sibling.register_hook(Arc::downgrade(&h_sibling) as Weak<dyn RevocationHook>);

        revoke_subtree(&child);

        assert_eq!(h_child.count(), 1);
        assert_eq!(h_grandchild.count(), 1);
        assert_eq!(h_sibling.count(), 0);
    }

    #[test]
    fn revoke_subtree_skips_dropped_hooks() {
        let root = RevNode::new_root();
        let hook = CountingHook::new();
        root.register_hook(Arc::downgrade(&hook) as Weak<dyn RevocationHook>);
        drop(hook);

        revoke_subtree(&root);
    }

    #[test]
    fn revoke_subtree_refires_live_hooks_on_repeat() {
        let root = RevNode::new_root();
        let child = RevNode::new_child(&root).unwrap();
        let hook = CountingHook::new();
        child.register_hook(Arc::downgrade(&hook) as Weak<dyn RevocationHook>);

        revoke_subtree(&root);
        assert_eq!(hook.count(), 1);

        revoke_subtree(&root);
        assert_eq!(hook.count(), 2);
    }

    #[test]
    fn new_child_dead_weaks_do_not_accumulate() {
        let root = RevNode::new_root();
        for _ in 0..10 {
            let _dropped = RevNode::new_child(&root);
        }

        let live = RevNode::new_child(&root).unwrap();
        let hook = CountingHook::new();
        live.register_hook(Arc::downgrade(&hook) as Weak<dyn RevocationHook>);

        revoke_subtree(&root);
        assert_eq!(hook.count(), 1);
    }

    #[test]
    fn register_hook_dead_weaks_do_not_accumulate() {
        let root = RevNode::new_root();

        for _ in 0..10 {
            let h = CountingHook::new();
            root.register_hook(Arc::downgrade(&h) as Weak<dyn RevocationHook>);
        }
        
        let live = CountingHook::new();
        root.register_hook(Arc::downgrade(&live) as Weak<dyn RevocationHook>);
        revoke_subtree(&root);
        assert_eq!(live.count(), 1);
    }

    #[test]
    fn new_child_after_revoke_is_rejected() {
        let root = RevNode::new_root();
        revoke_subtree(&root);
        assert!(RevNode::new_child(&root).is_none());
    }

    #[test]
    fn register_hook_after_revoke_fires_immediately() {
        let root = RevNode::new_root();
        revoke_subtree(&root);
        let hook = CountingHook::new();
        root.register_hook(Arc::downgrade(&hook) as Weak<dyn RevocationHook>);
        assert_eq!(hook.count(), 1);
    }
}
