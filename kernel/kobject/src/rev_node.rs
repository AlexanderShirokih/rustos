//! Граф деривации capability и каскадный отзыв.

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
}

impl RevNode {
    /// Создаёт корневой узел.
    pub fn new_root() -> Arc<Self> {
        Arc::new(Self {
            parent: None,
            children: MutexCell::new(Vec::new()),
            hooks: MutexCell::new(Vec::new()),
        })
    }

    /// Дочерний узел, производный от `parent`; регистрирует обратное ребро.
    pub fn new_child(parent: &Arc<RevNode>) -> Arc<Self> {
        let child = Arc::new(Self {
            parent: Some(Arc::downgrade(parent)),
            children: MutexCell::new(Vec::new()),
            hooks: MutexCell::new(Vec::new()),
        });
        parent
            .children
            .with_lock(|c| c.push(Arc::downgrade(&child)));
        child
    }

    /// Регистрирует `Weak` хука срыва (сильную ссылку держит сторона эффекта).
    pub fn register_hook(&self, hook: Weak<dyn RevocationHook>) {
        self.hooks.with_lock(|h| h.push(hook));
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

/// Каскадный отзыв поддерева: срывает hook'и узла, затем детей обходом вниз.
/// Зовётся из `HandleTable::remove`/`Drop` до дропа `Handle`. Идемпотентен:
/// повисшие `Weak` пропускаются.
pub fn revoke_subtree(node: &Arc<RevNode>) {
    // Итеративный обход worklist'ом, а не рекурсией: глубина дерева деривации
    // задаётся userland (цепочка duplicate), поэтому рекурсивный спуск грозил
    // бы переполнением стека ядра. Снимок hook'ов и детей берётся под локом, а
    // revoke() и спуск к детям выполняются вне лока.
    let mut stack: Vec<Arc<RevNode>> = Vec::new();
    stack.push(node.clone());
    while let Some(current) = stack.pop() {
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
        let child = RevNode::new_child(&root);
        assert!(child.is_alive());
    }

    #[test]
    fn child_dies_when_parent_dropped() {
        let root = RevNode::new_root();
        let child = RevNode::new_child(&root);
        drop(root);
        assert!(!child.is_alive());
    }

    #[test]
    fn grandchild_dies_through_dead_ancestor() {
        let root = RevNode::new_root();
        let mid = RevNode::new_child(&root);
        let leaf = RevNode::new_child(&mid);
        assert!(leaf.is_alive());
        drop(root);
        assert!(!leaf.is_alive());
        assert!(!mid.is_alive());
    }

    #[test]
    fn siblings_are_independent() {
        let root = RevNode::new_root();
        let a = RevNode::new_child(&root);
        let b = RevNode::new_child(&root);
        drop(a);

        assert!(b.is_alive());
    }

    #[test]
    fn revoke_subtree_fires_node_and_descendant_hooks_only() {
        let root = RevNode::new_root();
        let child = RevNode::new_child(&root);
        let grandchild = RevNode::new_child(&child);
        let sibling = RevNode::new_child(&root);

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
        let child = RevNode::new_child(&root);
        let hook = CountingHook::new();
        child.register_hook(Arc::downgrade(&hook) as Weak<dyn RevocationHook>);

        revoke_subtree(&root);
        assert_eq!(hook.count(), 1);

        revoke_subtree(&root);
        assert_eq!(hook.count(), 2);
    }
}
