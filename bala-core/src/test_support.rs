//! Test-only helpers shared by `bala-core`'s unit tests.

use std::cell::Cell;
use std::rc::Rc;

use crate::in_memory_store::InMemoryStore;
use crate::model::{Parents, Placement, Task, TaskId, TaskType, TreeFilter, User, UserId};
use crate::store::{Store, StoreError, StoreTx};

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

/// A [`Store`] over an [`InMemoryStore`] that counts every [`StoreTx`]
/// method call made through it, so a test can assert how much store work
/// an operation does. The counter is shared, so it stays readable after
/// the store moves into a `Core`.
pub(crate) struct CountingStore {
    inner: InMemoryStore,
    calls: Rc<Cell<usize>>,
}

impl CountingStore {
    /// Wraps a fresh [`InMemoryStore`] and returns it with a handle to
    /// its call counter.
    pub(crate) fn new() -> (Self, Rc<Cell<usize>>) {
        let calls = Rc::new(Cell::new(0));
        let store = Self {
            inner: InMemoryStore::default(),
            calls: Rc::clone(&calls),
        };
        (store, calls)
    }
}

impl Store for CountingStore {
    fn transaction<T>(
        &self,
        f: impl FnOnce(&mut dyn StoreTx) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.inner.transaction(|tx| {
            f(&mut CountingTx {
                inner: tx,
                calls: &self.calls,
            })
        })
    }
}

/// The [`StoreTx`] a [`CountingStore`] hands out: forwards every call to
/// the wrapped transaction and bumps the shared counter.
struct CountingTx<'a> {
    inner: &'a mut dyn StoreTx,
    calls: &'a Cell<usize>,
}

impl CountingTx<'_> {
    fn tick(&mut self) -> &mut dyn StoreTx {
        self.calls.set(self.calls.get() + 1);
        &mut *self.inner
    }
}

impl StoreTx for CountingTx<'_> {
    fn get_user(&mut self, id: UserId) -> Result<Option<User>, StoreError> {
        self.tick().get_user(id)
    }

    fn put_user(&mut self, user: &User) -> Result<(), StoreError> {
        self.tick().put_user(user)
    }

    fn list_users(&mut self) -> Result<Vec<User>, StoreError> {
        self.tick().list_users()
    }

    fn get_task(&mut self, id: TaskId) -> Result<Option<Task>, StoreError> {
        self.tick().get_task(id)
    }

    fn put_task(&mut self, task: &Task) -> Result<(), StoreError> {
        self.tick().put_task(task)
    }

    fn list_tasks(&mut self, filter: &TreeFilter) -> Result<Vec<Task>, StoreError> {
        self.tick().list_tasks(filter)
    }

    fn list_parent_edges(&mut self, id: TaskId) -> Result<Vec<TaskId>, StoreError> {
        self.tick().list_parent_edges(id)
    }

    fn replace_parent_edges(
        &mut self,
        child: TaskId,
        parents: &Parents,
        placement: Placement,
    ) -> Result<(), StoreError> {
        self.tick().replace_parent_edges(child, parents, placement)
    }

    fn swap_child_positions(
        &mut self,
        parent: Option<TaskId>,
        a: TaskId,
        b: TaskId,
    ) -> Result<(), StoreError> {
        self.tick().swap_child_positions(parent, a, b)
    }

    fn list_child_edges(&mut self, parent: Option<TaskId>) -> Result<Vec<TaskId>, StoreError> {
        self.tick().list_child_edges(parent)
    }

    fn list_all_child_edges(&mut self) -> Result<Vec<(Option<TaskId>, TaskId)>, StoreError> {
        self.tick().list_all_child_edges()
    }

    fn get_task_including_deleted(&mut self, id: TaskId) -> Result<Option<Task>, StoreError> {
        self.tick().get_task_including_deleted(id)
    }

    fn get_task_types(&mut self) -> Result<Vec<TaskType>, StoreError> {
        self.tick().get_task_types()
    }

    fn put_task_type(&mut self, t: &TaskType) -> Result<(), StoreError> {
        self.tick().put_task_type(t)
    }
}
