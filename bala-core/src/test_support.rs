//! Test-only helpers shared by `bala-core`'s unit tests.

use crate::model::TreeFilter;
use crate::store::{Store, StoreTx};

/// Asserts the parent-edge invariant over every task in `store`, live or
/// soft-deleted: each task has exactly one NULL-parent edge (it appears once
/// in `list_child_edges(None)` and has no real parent) or one or more real
/// parent edges (and is absent from the roots).
pub(crate) fn assert_edges_valid(store: &impl Store) {
    store
        .transaction(|tx: &mut dyn StoreTx| {
            let filter = TreeFilter {
                include_deleted: true,
                ..TreeFilter::default()
            };
            let mut root_counts = std::collections::HashMap::new();
            for root in tx.list_child_edges(None)? {
                *root_counts.entry(root).or_insert(0usize) += 1;
            }
            for task in tx.list_tasks(&filter)? {
                let null_edges = root_counts.get(&task.id).copied().unwrap_or(0);
                let real_edges = tx.list_parent_edges(task.id)?.len();
                assert!(
                    (null_edges == 1 && real_edges == 0) || (null_edges == 0 && real_edges >= 1),
                    "task {:?} has {null_edges} NULL edge(s) and {real_edges} real edge(s)",
                    task.id
                );
            }
            Ok(())
        })
        .unwrap();
}
