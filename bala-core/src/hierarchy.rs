//! Hierarchy invariants over the parent-edge DAG (LLD §Algorithm 1).
//!
//! This checkpoint (Story 1.1) only needs a seam: `create_task` attaches a
//! new, childless task to zero or more existing parents, which can never
//! create a cycle (a brand-new [`TaskId`] cannot already be an ancestor of
//! anything). [`check_new_parent`] is therefore a no-op today, but it is
//! the single function Story 3.1's `set_parents` will extend with the real
//! DFS/BFS cycle walk described in the LLD, so callers never need to
//! change once that check grows teeth.

use crate::model::TaskId;
use crate::store::{StoreError, StoreTx};

/// Checks that attaching `child` under `parent` would not violate the
/// hierarchy invariant (no task may be its own ancestor).
///
/// Currently always succeeds: at this checkpoint's scope, `child` is
/// always a freshly created [`TaskId`] with no existing edges, so it can
/// never already be an ancestor of `parent`. Story 3.1 extends this with
/// the real ancestor-reachability walk once `set_parents` can attach an
/// *existing* task (with its own descendants) under a new parent — at
/// that point this will need to report `CircularHierarchy` too, likely by
/// changing its return type; every call site already runs inside a
/// [`crate::Store::transaction`] closure, so that change stays local to
/// this function and its callers' error mapping.
///
/// Returns `Result` (rather than `()`) so callers propagate it with `?`
/// exactly like the future fallible version, and so
/// `create_task_should_run_hierarchy_check_for_each_given_parent` can
/// assert it is actually invoked once per parent.
///
/// # Errors
///
/// Returns `Err` only if the backend fails while walking ancestors (not
/// possible yet, since no walk happens today).
// Always `Ok` today is deliberate: this is the seam Story 3.1 extends
// with the real ancestor walk, so it stays fallible now rather than
// forcing every call site to change signature later.
#[allow(clippy::unnecessary_wraps)]
pub fn check_new_parent(
    _tx: &mut dyn StoreTx,
    _parent: TaskId,
    _child: TaskId,
) -> Result<(), StoreError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::in_memory_store::InMemoryStore;
    use crate::store::Store;

    #[test]
    fn check_new_parent_always_succeeds_for_a_fresh_child() {
        let store = InMemoryStore::default();
        let parent = TaskId::new();
        let child = TaskId::new();

        let result = store.transaction(|tx| check_new_parent(tx, parent, child));

        assert!(result.is_ok());
    }
}
