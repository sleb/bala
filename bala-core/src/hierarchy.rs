//! Hierarchy invariants over the parent-edge DAG (LLD §Algorithm 1).
//!
//! [`check_new_parent`] enforces the one hierarchy invariant this library
//! cares about: no task may be its own ancestor. Attaching `child` under
//! `parent` is rejected with [`CoreError::CircularHierarchy`] whenever
//! `child` is already reachable by walking upward from `parent` via
//! [`StoreTx::list_parent_edges`] — including the trivial case
//! `parent == child` (self-parenting). The walk is an explicit work-stack,
//! not recursion, matching `facade::tombstone_subtree`'s deep-chain safety:
//! `create_task` and `set_parents` place no limit
//! on hierarchy depth, so a user-built chain deep enough could overflow the
//! process stack if this recursed instead.

use std::collections::HashSet;

use crate::error::CoreError;
use crate::model::{Parents, Placement, TaskId};
use crate::store::StoreTx;

/// Checks that attaching `child` under `parent` would not violate the
/// hierarchy invariant (no task may be its own ancestor).
///
/// Walks upward from `parent`, one edge at a time via
/// [`StoreTx::list_parent_edges`], using an explicit work-stack seeded with
/// `parent` itself. A node's parents are only enqueued the first time that
/// node is visited (tracked via a `visited` set), so a shared ancestor
/// reached through two different paths in a DAG is walked exactly once —
/// this keeps the walk linear in the number of distinct ancestors rather
/// than exponential in the number of paths between them, and (combined with
/// the work-stack) lets it terminate on an arbitrarily long single chain
/// without re-walking already-seen nodes.
///
/// # Errors
///
/// - [`CoreError::CircularHierarchy`] if `child` is `parent` itself, or is
///   reachable as an ancestor of `parent` — attaching it would make `child`
///   its own ancestor.
/// - [`CoreError::Store`] if the backend fails while walking ancestors.
pub fn check_new_parent(
    tx: &mut dyn StoreTx,
    parent: TaskId,
    child: TaskId,
) -> Result<(), CoreError> {
    let mut visited = HashSet::new();
    let mut pending = vec![parent];

    while let Some(current) = pending.pop() {
        if !visited.insert(current) {
            continue;
        }

        if current == child {
            return Err(CoreError::CircularHierarchy {
                task: child,
                attempted_parent: parent,
            });
        }

        pending.extend(tx.list_parent_edges(current)?);
    }

    Ok(())
}

/// Sets `child`'s complete parent set to `parents`, after checking that no
/// new parent would make `child` its own ancestor.
///
/// This is the only place Core writes parent edges. Every newly added
/// parent is checked with [`check_new_parent`] before anything is written, so a
/// rejected call leaves the edges untouched.
///
/// # Errors
///
/// - [`CoreError::CircularHierarchy`] if any parent in `parents` is `child`
///   or a descendant of it.
/// - [`CoreError::Store`] if the backend fails.
pub fn replace_parents(
    tx: &mut dyn StoreTx,
    child: TaskId,
    parents: &Parents,
    placement: Placement,
) -> Result<(), CoreError> {
    // Only parents `child` doesn't already have can introduce a cycle, so a
    // pure removal (e.g. deleting a task in a deep chain) skips the
    // upward walk entirely.
    let current = tx.list_parent_edges(child)?;
    for &parent in parents.ids() {
        if !current.contains(&parent) {
            check_new_parent(tx, parent, child)?;
        }
    }
    tx.replace_parent_edges(child, parents, placement)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::in_memory_store::InMemoryStore;
    use crate::model::{Parents, Placement};
    use crate::store::{Store, StoreError};

    /// Adds `parent` to `child`'s existing parents (test-only graph builder).
    fn attach(tx: &mut dyn StoreTx, parent: TaskId, child: TaskId) -> Result<(), StoreError> {
        let mut parents = tx.list_parent_edges(child)?;
        parents.push(parent);
        tx.replace_parent_edges(child, &Parents::Under(parents), Placement::End)
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
    fn check_new_parent_should_allow_a_shared_ancestor_reached_via_two_paths() {
        // grandparent G has two children P1 and P2, and `task` already has
        // both P1 and P2 as parents (a DAG diamond). Walking upward from
        // either P1 or P2 reaches G, but G is not `task` itself, so
        // attaching `task` under a *new* parent that is itself a child of
        // G must still succeed — reaching G via two paths must not be
        // mistaken for a cycle.
        let store = InMemoryStore::default();
        let grandparent = TaskId::new();
        let parent_1 = TaskId::new();
        let parent_2 = TaskId::new();
        let task = TaskId::new();
        let new_parent = TaskId::new();

        store
            .transaction(|tx| {
                attach(tx, grandparent, parent_1)?;
                attach(tx, grandparent, parent_2)?;
                attach(tx, parent_1, task)?;
                attach(tx, parent_2, task)?;
                attach(tx, grandparent, new_parent)
            })
            .unwrap();

        // Attaching `task` under `new_parent` walks new_parent -> grandparent,
        // never encountering `task`, so it succeeds despite grandparent
        // being reachable from `task` via two separate paths.
        let result = store
            .transaction(|tx| Ok(check_new_parent(tx, new_parent, task)))
            .unwrap();

        assert!(result.is_ok());
    }

    #[test]
    fn check_new_parent_should_not_overflow_the_stack_on_a_deep_ancestor_chain() {
        // A chain of 2000+ tasks, each parented under the previous one,
        // built directly via the store (faster than 2000 real
        // `create_task` calls and exercises the same store contract).
        // Attaching the far end of the chain back onto the chain's start
        // must be rejected as circular, and must not overflow the stack —
        // regression-testing the explicit work-stack walk.
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
mod replace_parents_tests {
    use super::*;
    use crate::in_memory_store::InMemoryStore;
    use crate::store::Store;

    #[test]
    fn replace_parents_should_write_edges_when_no_cycle() {
        let store = InMemoryStore::default();
        let (parent, child) = (TaskId::new(), TaskId::new());

        store
            .transaction(|tx| {
                Ok(replace_parents(
                    tx,
                    child,
                    &Parents::Under(vec![parent]),
                    Placement::End,
                ))
            })
            .unwrap()
            .unwrap();

        let edges = store.transaction(|tx| tx.list_parent_edges(child)).unwrap();
        assert_eq!(edges, vec![parent]);
    }

    #[test]
    fn replace_parents_should_skip_cycle_walk_for_pure_removal() {
        // Removing a parent is written without an ancestor walk.
        let store = InMemoryStore::default();
        let (a, b) = (TaskId::new(), TaskId::new());
        store
            .transaction(|tx| tx.replace_parent_edges(b, &Parents::Under(vec![a]), Placement::End))
            .unwrap();

        store
            .transaction(|tx| Ok(replace_parents(tx, b, &Parents::TopLevel, Placement::End)))
            .unwrap()
            .unwrap();

        let edges = store.transaction(|tx| tx.list_parent_edges(b)).unwrap();
        assert!(edges.is_empty());
    }

    #[test]
    fn replace_parents_should_reject_cycle_without_writing() {
        let store = InMemoryStore::default();
        let (a, b) = (TaskId::new(), TaskId::new());
        store
            .transaction(|tx| tx.replace_parent_edges(b, &Parents::Under(vec![a]), Placement::End))
            .unwrap();

        let result = store
            .transaction(|tx| {
                Ok(replace_parents(
                    tx,
                    a,
                    &Parents::Under(vec![b]),
                    Placement::End,
                ))
            })
            .unwrap();

        assert!(matches!(result, Err(CoreError::CircularHierarchy { .. })));
        let edges = store.transaction(|tx| tx.list_parent_edges(a)).unwrap();
        assert!(edges.is_empty());
    }
}
