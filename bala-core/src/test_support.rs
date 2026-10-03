//! Test-only helpers shared by `bala-core`'s unit tests.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::in_memory_store::InMemoryStore;
use crate::model::{
    Dependency, DependencyType, Placement, Task, TaskId, TaskType, TreeFilter, User, UserId,
};
use crate::store::{Store, StoreError, StoreTx};

/// Asserts the parent-edge invariant over every task in `store`, live or
/// soft-deleted: each task has exactly one parent edge. Either it is the
/// NULL-parent edge (the task appears once in `list_child_edges(None)` and
/// has no real parent), or it is a real one (the task is absent from the
/// roots and appears once among its parent's children).
pub(crate) fn assert_edges_valid(store: &impl Store) {
    store
        .transaction(|tx: &mut dyn StoreTx| {
            let filter = TreeFilter {
                include_deleted: true,
                ..TreeFilter::default()
            };
            // Per child: how many NULL-parent edges and how many real ones.
            let mut edge_counts = std::collections::HashMap::new();
            for (parent, child) in tx.list_all_child_edges()? {
                let (null_edges, real_edges) = edge_counts.entry(child).or_insert((0usize, 0usize));
                match parent {
                    None => *null_edges += 1,
                    Some(_) => *real_edges += 1,
                }
            }
            for task in tx.list_tasks(&filter)? {
                let parent = tx.get_parent_edge(task.id)?;
                let (null_edges, real_edges) = edge_counts.get(&task.id).copied().unwrap_or((0, 0));
                assert!(
                    (parent.is_none() && null_edges == 1 && real_edges == 0)
                        || (parent.is_some() && null_edges == 0 && real_edges == 1),
                    "task {:?} has parent {parent:?}, {null_edges} NULL edge(s) and \
                     {real_edges} real edge(s)",
                    task.id
                );
            }
            Ok(())
        })
        .unwrap();
}

/// The call log a [`CountingStore`] records: one entry per [`StoreTx`]
/// call, holding the 0-based index of the `Store::transaction` call it ran
/// in and the method's name.
pub(crate) type CallLog = Rc<RefCell<Vec<(usize, &'static str)>>>;

/// A [`Store`] over an [`InMemoryStore`] that logs every [`StoreTx`]
/// method call made through it, with the transaction it ran in, so a test
/// can assert how much store work an operation does (the log's length) and
/// which calls share a transaction. The log is shared, so a handle taken
/// via [`CountingStore::log`] stays readable after the store moves into a
/// `Core`.
pub(crate) struct CountingStore {
    inner: InMemoryStore,
    transactions: Cell<usize>,
    log: CallLog,
}

impl CountingStore {
    /// Wraps a fresh [`InMemoryStore`] with an empty call log.
    pub(crate) fn new() -> Self {
        Self {
            inner: InMemoryStore::default(),
            transactions: Cell::new(0),
            log: CallLog::default(),
        }
    }

    /// Returns a handle to the log of `(transaction index, method name)`
    /// pairs, one per [`StoreTx`] call, in call order.
    pub(crate) fn log(&self) -> CallLog {
        Rc::clone(&self.log)
    }
}

impl Store for CountingStore {
    fn transaction<T>(
        &self,
        f: impl FnOnce(&mut dyn StoreTx) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let tx_index = self.transactions.get();
        self.transactions.set(tx_index + 1);
        self.inner.transaction(|tx| {
            f(&mut CountingTx {
                inner: tx,
                tx_index,
                log: &self.log,
            })
        })
    }
}

/// The [`StoreTx`] a [`CountingStore`] hands out: forwards every call to
/// the wrapped transaction and logs the call under this transaction's
/// index.
struct CountingTx<'a> {
    inner: &'a mut dyn StoreTx,
    tx_index: usize,
    log: &'a RefCell<Vec<(usize, &'static str)>>,
}

impl CountingTx<'_> {
    /// Logs a call to `method` under this transaction's index and returns
    /// the wrapped transaction to forward the call to.
    fn tick(&mut self, method: &'static str) -> &mut dyn StoreTx {
        self.log.borrow_mut().push((self.tx_index, method));
        &mut *self.inner
    }
}

impl StoreTx for CountingTx<'_> {
    fn get_user(&mut self, id: UserId) -> Result<Option<User>, StoreError> {
        self.tick("get_user").get_user(id)
    }

    fn put_user(&mut self, user: &User) -> Result<(), StoreError> {
        self.tick("put_user").put_user(user)
    }

    fn list_users(&mut self) -> Result<Vec<User>, StoreError> {
        self.tick("list_users").list_users()
    }

    fn get_task(&mut self, id: TaskId) -> Result<Option<Task>, StoreError> {
        self.tick("get_task").get_task(id)
    }

    fn put_task(&mut self, task: &Task) -> Result<(), StoreError> {
        self.tick("put_task").put_task(task)
    }

    fn list_tasks(&mut self, filter: &TreeFilter) -> Result<Vec<Task>, StoreError> {
        self.tick("list_tasks").list_tasks(filter)
    }

    fn get_parent_edge(&mut self, id: TaskId) -> Result<Option<TaskId>, StoreError> {
        self.tick("get_parent_edge").get_parent_edge(id)
    }

    fn set_parent_edge(
        &mut self,
        child: TaskId,
        parent: Option<TaskId>,
        placement: Placement,
    ) -> Result<(), StoreError> {
        self.tick("set_parent_edge")
            .set_parent_edge(child, parent, placement)
    }

    fn swap_child_positions(
        &mut self,
        parent: Option<TaskId>,
        a: TaskId,
        b: TaskId,
    ) -> Result<(), StoreError> {
        self.tick("swap_child_positions")
            .swap_child_positions(parent, a, b)
    }

    fn list_child_edges(&mut self, parent: Option<TaskId>) -> Result<Vec<TaskId>, StoreError> {
        self.tick("list_child_edges").list_child_edges(parent)
    }

    fn list_all_child_edges(&mut self) -> Result<Vec<(Option<TaskId>, TaskId)>, StoreError> {
        self.tick("list_all_child_edges").list_all_child_edges()
    }

    fn list_dependency_edges(&mut self, id: TaskId) -> Result<Vec<Dependency>, StoreError> {
        self.tick("list_dependency_edges").list_dependency_edges(id)
    }

    fn list_successor_edges(&mut self, id: TaskId) -> Result<Vec<TaskId>, StoreError> {
        self.tick("list_successor_edges").list_successor_edges(id)
    }

    fn add_dependency_edge(
        &mut self,
        predecessor: TaskId,
        successor: TaskId,
        dep_type: DependencyType,
    ) -> Result<(), StoreError> {
        self.tick("add_dependency_edge")
            .add_dependency_edge(predecessor, successor, dep_type)
    }

    fn remove_dependency_edge(
        &mut self,
        predecessor: TaskId,
        successor: TaskId,
    ) -> Result<(), StoreError> {
        self.tick("remove_dependency_edge")
            .remove_dependency_edge(predecessor, successor)
    }

    fn get_task_including_deleted(&mut self, id: TaskId) -> Result<Option<Task>, StoreError> {
        self.tick("get_task_including_deleted")
            .get_task_including_deleted(id)
    }

    fn get_task_types(&mut self) -> Result<Vec<TaskType>, StoreError> {
        self.tick("get_task_types").get_task_types()
    }

    fn put_task_type(&mut self, t: &TaskType) -> Result<(), StoreError> {
        self.tick("put_task_type").put_task_type(t)
    }
}
