//! Hierarchy invariants over the parent-edge tree (LLD §Algorithm 1).
//!
//! [`check_new_parent`] enforces the one hierarchy invariant this library
//! cares about: no task may be its own ancestor. Attaching `child` under
//! `parent` is rejected with [`CoreError::CircularHierarchy`] whenever
//! `child` is `parent` itself or one of `parent`'s ancestors, found by
//! following [`StoreTx::get_parent_edge`] upward from `parent`. A task has
//! at most one parent, so that walk is a plain loop over one chain — never
//! recursion: `create_task` and `set_parent` place no limit on hierarchy
//! depth, so a user-built chain deep enough could overflow the process
//! stack if this recursed instead.

use crate::error::CoreError;
use crate::model::{Placement, TaskId};
use crate::store::StoreTx;

/// Checks that attaching `child` under `parent` would not violate the
/// hierarchy invariant (no task may be its own ancestor).
///
/// Follows the single parent chain upward from `parent`, one edge at a time
/// via [`StoreTx::get_parent_edge`], starting with `parent` itself and
/// stopping at the top level. The chain is finite: no write ever makes a
/// task its own ancestor.
///
/// # Errors
///
/// - [`CoreError::CircularHierarchy`] if `child` is `parent` itself, or is
///   an ancestor of `parent` — attaching it would make `child` its own
///   ancestor.
/// - [`CoreError::Store`] if the backend fails while walking ancestors.
pub fn check_new_parent(
    tx: &mut dyn StoreTx,
    parent: TaskId,
    child: TaskId,
) -> Result<(), CoreError> {
    let mut current = Some(parent);

    while let Some(ancestor) = current {
        if ancestor == child {
            return Err(CoreError::CircularHierarchy {
                task: child,
                attempted_parent: parent,
            });
        }
        current = tx.get_parent_edge(ancestor)?;
    }

    Ok(())
}

/// Moves `child` under `parent` (`None` = the top level), after checking
/// that the new parent would not make `child` its own ancestor.
///
/// Every write that puts an existing task under a parent goes through it.
/// Two writes skip it because they cannot form a cycle: `create_task`
/// writes a brand-new task's edge directly, since a new id has no
/// descendants, and a subtree delete moves each descendant's edge straight
/// to the top level. The check runs before anything
/// is written, so a rejected call leaves the edge untouched. A move to the
/// top level, or to the parent `child` already has, cannot create a cycle
/// and skips the upward walk.
///
/// # Errors
///
/// - [`CoreError::CircularHierarchy`] if `parent` is `child` or a descendant
///   of it.
/// - [`CoreError::Store`] if the backend fails.
pub fn set_parent(
    tx: &mut dyn StoreTx,
    child: TaskId,
    parent: Option<TaskId>,
    placement: Placement,
) -> Result<(), CoreError> {
    if let Some(new_parent) = parent
        && tx.get_parent_edge(child)? != parent
    {
        check_new_parent(tx, new_parent, child)?;
    }
    tx.set_parent_edge(child, parent, placement)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::in_memory_store::InMemoryStore;
    use crate::store::{Store, StoreError};
    use crate::test_support::CountingStore;

    /// Puts `child` under `parent` (test-only tree builder).
    fn attach(tx: &mut dyn StoreTx, parent: TaskId, child: TaskId) -> Result<(), StoreError> {
        tx.set_parent_edge(child, Some(parent), Placement::End)
    }

    #[test]
    fn check_new_parent_should_allow_attaching_to_a_task_with_no_existing_ancestors() {
        let store = InMemoryStore::default();
        let parent = TaskId::new();
        let child = TaskId::new();

        let result = store.transaction(|tx| Ok(check_new_parent(tx, parent, child)));

        assert!(result.unwrap().is_ok());
    }

    #[test]
    fn check_new_parent_should_reject_self_parenting() {
        let store = InMemoryStore::default();
        let task = TaskId::new();

        let result = store
            .transaction(|tx| Ok(check_new_parent(tx, task, task)))
            .unwrap();

        assert!(matches!(
            result,
            Err(CoreError::CircularHierarchy { task: t, attempted_parent })
                if t == task && attempted_parent == task
        ));
    }

    #[test]
    fn check_new_parent_should_reject_when_child_is_an_ancestor_of_parent() {
        // grandparent -> parent -> (attempting) child, but `child` is
        // actually `grandparent`: attaching it under `parent` would make it
        // its own ancestor.
        let store = InMemoryStore::default();
        let grandparent = TaskId::new();
        let parent = TaskId::new();

        store
            .transaction(|tx| attach(tx, grandparent, parent))
            .unwrap();

        let result = store
            .transaction(|tx| Ok(check_new_parent(tx, parent, grandparent)))
            .unwrap();

        assert!(matches!(
            result,
            Err(CoreError::CircularHierarchy { task, attempted_parent })
                if task == grandparent && attempted_parent == parent
        ));
    }

    #[test]
    fn check_new_parent_should_walk_one_parent_chain() {
        // root -> a -> b -> c, with `sibling` beside `a` under `root`. The
        // walk up from `c` asks for the parent of each task on c's own
        // chain and of nothing else: `sibling`'s subtree is never read.
        let store = CountingStore::new();
        let log = store.log();
        let (root, a, b, c) = (TaskId::new(), TaskId::new(), TaskId::new(), TaskId::new());
        let (sibling, unrelated) = (TaskId::new(), TaskId::new());
        store
            .transaction(|tx| {
                tx.set_parent_edge(root, None, Placement::End)?;
                attach(tx, root, a)?;
                attach(tx, root, sibling)?;
                attach(tx, a, b)?;
                attach(tx, b, c)
            })
            .unwrap();
        let start = log.borrow().len();

        let allowed = store
            .transaction(|tx| Ok(check_new_parent(tx, c, unrelated)))
            .unwrap();
        let calls: Vec<&str> = log.borrow()[start..].iter().map(|&(_, m)| m).collect();
        let on_chain = store
            .transaction(|tx| Ok(check_new_parent(tx, c, a)))
            .unwrap();
        let off_chain = store
            .transaction(|tx| Ok(check_new_parent(tx, c, sibling)))
            .unwrap();

        // One lookup each for c, b, a and root.
        assert!(allowed.is_ok());
        assert_eq!(calls, ["get_parent_edge"; 4]);
        assert!(matches!(
            on_chain,
            Err(CoreError::CircularHierarchy { task, attempted_parent })
                if task == a && attempted_parent == c
        ));
        assert!(off_chain.is_ok());
    }

    #[test]
    fn check_new_parent_should_not_overflow_the_stack_on_a_deep_ancestor_chain() {
        // A chain of 2000+ tasks, each parented under the previous one,
        // built directly via the store (faster than 2000 real
        // `create_task` calls and exercises the same store contract).
        // Attaching the far end of the chain back onto the chain's start
        // must be rejected as circular, and must not overflow the stack —
        // regression-testing that the walk is a loop, not recursion.
        let store = InMemoryStore::default();
        let root = TaskId::new();
        let mut current = root;
        let mut chain_end = root;
        store
            .transaction(|tx| {
                for _ in 0..3000 {
                    let next = TaskId::new();
                    attach(tx, current, next)?;
                    current = next;
                }
                chain_end = current;
                Ok(())
            })
            .unwrap();

        // Attaching `root` under `chain_end` would make `root` its own
        // ancestor, since `root` is already an ancestor of `chain_end`.
        let result = store
            .transaction(|tx| Ok(check_new_parent(tx, chain_end, root)))
            .unwrap();

        assert!(matches!(
            result,
            Err(CoreError::CircularHierarchy { task, attempted_parent })
                if task == root && attempted_parent == chain_end
        ));
    }
}

#[cfg(test)]
mod set_parent_tests {
    use super::*;
    use crate::in_memory_store::InMemoryStore;
    use crate::store::Store;
    use crate::test_support::CountingStore;

    #[test]
    fn set_parent_should_write_edge_when_no_cycle() {
        let store = InMemoryStore::default();
        let (parent, child) = (TaskId::new(), TaskId::new());

        store
            .transaction(|tx| Ok(set_parent(tx, child, Some(parent), Placement::End)))
            .unwrap()
            .unwrap();

        let edge = store.transaction(|tx| tx.get_parent_edge(child)).unwrap();
        assert_eq!(edge, Some(parent));
    }

    #[test]
    fn set_parent_should_skip_cycle_walk_when_moving_to_top_level() {
        // A move to the top level is written without an ancestor walk.
        let store = CountingStore::new();
        let log = store.log();
        let (a, b) = (TaskId::new(), TaskId::new());
        store
            .transaction(|tx| tx.set_parent_edge(b, Some(a), Placement::End))
            .unwrap();
        let start = log.borrow().len();

        store
            .transaction(|tx| Ok(set_parent(tx, b, None, Placement::End)))
            .unwrap()
            .unwrap();

        let calls: Vec<&str> = log.borrow()[start..].iter().map(|&(_, m)| m).collect();
        assert_eq!(calls, ["set_parent_edge"]);
        let edge = store.transaction(|tx| tx.get_parent_edge(b)).unwrap();
        assert_eq!(edge, None);
    }

    #[test]
    fn set_parent_should_skip_cycle_walk_when_parent_is_unchanged() {
        let store = CountingStore::new();
        let log = store.log();
        let (root, a, b) = (TaskId::new(), TaskId::new(), TaskId::new());
        store
            .transaction(|tx| {
                tx.set_parent_edge(a, Some(root), Placement::End)?;
                tx.set_parent_edge(b, Some(a), Placement::End)
            })
            .unwrap();
        let start = log.borrow().len();

        store
            .transaction(|tx| Ok(set_parent(tx, b, Some(a), Placement::End)))
            .unwrap()
            .unwrap();

        // One lookup for `b`'s current parent, none up `a`'s chain.
        let calls: Vec<&str> = log.borrow()[start..].iter().map(|&(_, m)| m).collect();
        assert_eq!(calls, ["get_parent_edge", "set_parent_edge"]);
    }

    #[test]
    fn set_parent_should_reject_cycle_without_writing() {
        let store = InMemoryStore::default();
        let (a, b) = (TaskId::new(), TaskId::new());
        store
            .transaction(|tx| tx.set_parent_edge(b, Some(a), Placement::End))
            .unwrap();

        let result = store
            .transaction(|tx| Ok(set_parent(tx, a, Some(b), Placement::End)))
            .unwrap();

        assert!(matches!(result, Err(CoreError::CircularHierarchy { .. })));
        let edge = store.transaction(|tx| tx.get_parent_edge(a)).unwrap();
        assert_eq!(edge, None);
        let children = store
            .transaction(|tx| tx.list_child_edges(Some(b)))
            .unwrap();
        assert_eq!(children, []);
    }
}
