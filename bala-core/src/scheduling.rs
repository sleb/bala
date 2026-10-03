//! Dependency invariants over the dependency-edge graph (LLD §Algorithm 2).
//!
//! [`check_new_dependency`] is the gate every new dependency edge between
//! two distinct tasks passes before it is written; the caller rejects a
//! self-dependency first. It enforces two invariants: no task depends on
//! its own ancestor or descendant in the hierarchy, and no chain of
//! dependencies leads back to where it started. Every walk is an explicit
//! work-stack, not recursion, as in
//! [`hierarchy::check_new_parent`](crate::hierarchy::check_new_parent):
//! hierarchy depth and dependency chains are unbounded, so a long enough
//! chain could overflow the process stack if they recursed.

use std::collections::{HashMap, HashSet};

use crate::error::CoreError;
use crate::model::TaskId;
use crate::store::{StoreError, StoreTx};

/// Checks that making `id` depend on `predecessor`, a different task,
/// would not violate a dependency invariant. The caller rejects
/// `id == predecessor` with [`CoreError::SelfDependency`] before calling
/// this.
///
/// Both hierarchy checks walk upward through every parent of every task
/// via [`StoreTx::list_parent_edges`]: from `id` looking for `predecessor`
/// (an ancestor), then from `predecessor` looking for `id` (a descendant
/// of `id` is exactly a task from which `id` is reachable upward). Each
/// walk visits only the ancestors of its start, never `id`'s whole
/// subtree. Then walks the dependency graph from `predecessor` through
/// what it already depends on, transitively, via
/// [`StoreTx::list_dependency_edges`], and rejects if `id` turns up: the
/// new edge would close a cycle. That walk ignores dependency types, and
/// since soft-deleted tasks keep their edges, every walk passes through
/// them. Whether either task exists is the caller's concern.
///
/// # Errors
///
/// - [`CoreError::DependsOnRelative`] if `predecessor` is an ancestor or a
///   descendant of `id`.
/// - [`CoreError::CircularDependency`] if `predecessor` already depends on
///   `id`, directly or transitively; `cycle` is `[id, predecessor, …, id]`,
///   each entry depending on the next.
/// - [`CoreError::Store`] if the backend fails while walking either graph.
pub fn check_new_dependency(
    tx: &mut dyn StoreTx,
    id: TaskId,
    predecessor: TaskId,
) -> Result<(), CoreError> {
    let up = |tx: &mut dyn StoreTx, task: TaskId| tx.list_parent_edges(task);
    let is_ancestor = find_path(tx, id, predecessor, up)?.is_some();
    let is_descendant = !is_ancestor && find_path(tx, predecessor, id, up)?.is_some();
    if is_ancestor || is_descendant {
        return Err(CoreError::DependsOnRelative {
            task: id,
            other: predecessor,
        });
    }

    let dependency_path = find_path(tx, predecessor, id, |tx, task| {
        Ok(tx
            .list_dependency_edges(task)?
            .into_iter()
            .map(|dependency| dependency.predecessor_id)
            .collect())
    })?;
    if let Some(path) = dependency_path {
        let mut cycle = Vec::with_capacity(path.len() + 1);
        cycle.push(id);
        cycle.extend(path);
        return Err(CoreError::CircularDependency { cycle });
    }

    Ok(())
}

/// Finds a path from `start` to `target` by following `neighbors` one edge
/// at a time, returning it as `[start, …, target]`, or `None` if `target`
/// is unreachable.
///
/// The walk is an explicit work-stack seeded with `start`. Each task is
/// enqueued only the first time it is discovered, and the task it was
/// discovered from is recorded then, so a task reached through several
/// paths is walked exactly once and every recorded step is a real edge. The
/// walk stops as soon as `target` turns up, and the path is rebuilt by
/// following the recorded steps back to `start`.
fn find_path(
    tx: &mut dyn StoreTx,
    start: TaskId,
    target: TaskId,
    neighbors: impl Fn(&mut dyn StoreTx, TaskId) -> Result<Vec<TaskId>, StoreError>,
) -> Result<Option<Vec<TaskId>>, StoreError> {
    let mut discovered = HashSet::from([start]);
    let mut discovered_from = HashMap::new();
    let mut pending = vec![start];

    while let Some(current) = pending.pop() {
        if current == target {
            let mut path = vec![current];
            let mut node = current;
            while let Some(&previous) = discovered_from.get(&node) {
                path.push(previous);
                node = previous;
            }
            path.reverse();
            return Ok(Some(path));
        }

        for next in neighbors(tx, current)? {
            if discovered.insert(next) {
                discovered_from.insert(next, current);
                pending.push(next);
            }
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::in_memory_store::InMemoryStore;
    use crate::store::Store;

    #[test]
    fn check_new_dependency_should_accept_two_distinct_tasks() {
        let store = InMemoryStore::default();
        let task = TaskId::new();
        let predecessor = TaskId::new();

        let result = store
            .transaction(|tx| Ok(check_new_dependency(tx, task, predecessor)))
            .unwrap();

        assert!(result.is_ok());
    }
}
