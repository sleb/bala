//! [`SqliteStore`]/`SqliteTx`: the `Store`/`StoreTx` implementation (LLD-2
//! §`Store`/`StoreTx` Implementation, §Decision).

use std::cell::RefCell;
use std::path::Path;

use bala_core::{Store, StoreError, StoreTx, Task, TaskId, TaskType, TreeFilter};
use rusqlite::Connection;

use crate::{edges, schema, task, types};

/// SQLite-backed [`Store`].
///
/// Holds a single [`Connection`] behind a [`RefCell`] rather than a
/// `Mutex`: `Store::transaction` takes `&self` while
/// `rusqlite::Connection::transaction()` needs `&mut Connection`, and
/// `RefCell` bridges that without changing `bala-core`'s already-committed
/// trait signature. Safe under `bala-core`'s single-writer-at-a-time
/// assumption; a `RefCell` panics on reentrant borrow (nested
/// `Store::transaction` calls) rather than deadlocking or interleaving.
/// `SqliteStore` is therefore `!Sync` by construction — a future
/// multi-threaded caller wraps it in its own `Mutex` rather than this crate
/// paying for thread safety no current caller needs.
#[derive(Debug)]
pub struct SqliteStore {
    conn: RefCell<Connection>,
}

impl SqliteStore {
    /// Opens (creating if absent) the SQLite file at `path`, applying any
    /// pending migrations and seeding the default `"task"` `TaskType`.
    /// `path` is caller-supplied — this crate has no opinion on config-dir
    /// conventions.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the file can't be opened, or if migrations fail.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path).map_err(task::sqlite_err)?;
        Self::init(conn, true)
    }

    /// In-memory (`:memory:`) store — same migrations, no file. Intended
    /// for tests.
    ///
    /// # Errors
    ///
    /// Returns `Err` if migrations fail.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory().map_err(task::sqlite_err)?;
        Self::init(conn, false)
    }

    /// Shared setup for both constructors: pragmas, then migrations (which
    /// also seed the default task type — see `migrations/V1__init.sql`).
    fn init(mut conn: Connection, wal: bool) -> Result<Self, StoreError> {
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(task::sqlite_err)?;
        if wal {
            conn.pragma_update(None, "journal_mode", "WAL")
                .map_err(task::sqlite_err)?;
        }
        schema::runner()
            .run(&mut conn)
            .map_err(|err| StoreError::Backend(err.to_string()))?;
        Ok(Self {
            conn: RefCell::new(conn),
        })
    }
}

impl Store for SqliteStore {
    fn transaction<T>(
        &self,
        f: impl FnOnce(&mut dyn StoreTx) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let mut conn = self.conn.borrow_mut();
        let tx = conn.transaction().map_err(task::sqlite_err)?;
        let mut store_tx = SqliteTx { tx };
        let result = f(&mut store_tx)?;
        store_tx.tx.commit().map_err(task::sqlite_err)?;
        Ok(result)
    }
}

struct SqliteTx<'a> {
    tx: rusqlite::Transaction<'a>,
}

impl StoreTx for SqliteTx<'_> {
    fn get_task(&mut self, id: TaskId) -> Result<Option<Task>, StoreError> {
        task::get_task(&self.tx, id)
    }

    fn put_task(&mut self, task: &Task) -> Result<(), StoreError> {
        task::put_task(&self.tx, task)
    }

    fn list_tasks(&mut self, filter: &TreeFilter) -> Result<Vec<Task>, StoreError> {
        task::list_tasks(&self.tx, filter)
    }

    fn list_parent_edges(&mut self, id: TaskId) -> Result<Vec<TaskId>, StoreError> {
        edges::list_parent_edges(&self.tx, id)
    }

    fn add_parent_edge(&mut self, parent: TaskId, child: TaskId) -> Result<(), StoreError> {
        edges::add_parent_edge(&self.tx, parent, child)
    }

    fn get_task_types(&mut self) -> Result<Vec<TaskType>, StoreError> {
        types::get_task_types(&self.tx)
    }

    fn put_task_type(&mut self, t: &TaskType) -> Result<(), StoreError> {
        types::put_task_type(&self.tx, t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bala_core::TaskStatus;
    use chrono::Utc;
    use uuid::Uuid;

    fn sample_task(type_key: &str, status: TaskStatus) -> Task {
        let now = Utc::now();
        Task {
            id: TaskId::new(),
            title: "Title".to_owned(),
            description: None,
            parent_ids: Vec::new(),
            type_key: type_key.to_owned(),
            status,
            start_date: None,
            due_date: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn goal_type() -> TaskType {
        TaskType {
            key: "goal".to_owned(),
            label: "Goal".to_owned(),
            color: None,
            sort_order: 1,
        }
    }

    #[test]
    fn open_in_memory_seeds_default_task_type() {
        let store = SqliteStore::open_in_memory().unwrap();
        let types = store.transaction(|tx| tx.get_task_types()).unwrap();
        assert!(types.iter().any(|t| t.key == "task"));
    }

    #[test]
    fn put_task_then_get_task_returns_same_task() {
        let store = SqliteStore::open_in_memory().unwrap();
        let task = sample_task("task", TaskStatus::Incomplete);

        store
            .transaction(|tx| {
                tx.put_task(&task)?;
                Ok(())
            })
            .unwrap();

        let fetched = store.transaction(|tx| tx.get_task(task.id)).unwrap();
        assert_eq!(fetched, Some(task));
    }

    #[test]
    fn get_task_for_unknown_id_returns_none() {
        let store = SqliteStore::open_in_memory().unwrap();
        let fetched = store.transaction(|tx| tx.get_task(TaskId::new())).unwrap();
        assert_eq!(fetched, None);
    }

    #[test]
    fn list_tasks_with_no_filter_returns_all_tasks() {
        let store = SqliteStore::open_in_memory().unwrap();
        store
            .transaction(|tx| tx.put_task_type(&goal_type()))
            .unwrap();
        let a = sample_task("task", TaskStatus::Incomplete);
        let b = sample_task("goal", TaskStatus::Complete);

        store
            .transaction(|tx| {
                tx.put_task(&a)?;
                tx.put_task(&b)?;
                Ok(())
            })
            .unwrap();

        let mut listed = store
            .transaction(|tx| tx.list_tasks(&TreeFilter::default()))
            .unwrap();
        listed.sort_by_key(|t| Uuid::from(t.id));
        let mut expected = vec![a, b];
        expected.sort_by_key(|t| Uuid::from(t.id));
        assert_eq!(listed, expected);
    }

    #[test]
    fn list_tasks_filters_by_type_key() {
        let store = SqliteStore::open_in_memory().unwrap();
        store
            .transaction(|tx| tx.put_task_type(&goal_type()))
            .unwrap();
        let goal = sample_task("goal", TaskStatus::Incomplete);
        let task = sample_task("task", TaskStatus::Incomplete);

        store
            .transaction(|tx| {
                tx.put_task(&goal)?;
                tx.put_task(&task)?;
                Ok(())
            })
            .unwrap();

        let filter = TreeFilter {
            type_key: Some("goal".to_owned()),
            ..TreeFilter::default()
        };
        let listed = store.transaction(|tx| tx.list_tasks(&filter)).unwrap();
        assert_eq!(listed, vec![goal]);
    }

    #[test]
    fn list_tasks_filters_by_status() {
        let store = SqliteStore::open_in_memory().unwrap();
        let done = sample_task("task", TaskStatus::Complete);
        let not_done = sample_task("task", TaskStatus::Incomplete);

        store
            .transaction(|tx| {
                tx.put_task(&done)?;
                tx.put_task(&not_done)?;
                Ok(())
            })
            .unwrap();

        let filter = TreeFilter {
            status: Some(TaskStatus::Complete),
            ..TreeFilter::default()
        };
        let listed = store.transaction(|tx| tx.list_tasks(&filter)).unwrap();
        assert_eq!(listed, vec![done]);
    }

    #[test]
    fn list_parent_edges_for_task_with_no_parents_returns_empty() {
        let store = SqliteStore::open_in_memory().unwrap();
        let edges = store
            .transaction(|tx| tx.list_parent_edges(TaskId::new()))
            .unwrap();
        assert!(edges.is_empty());
    }

    // Unlike the `InMemoryStore` equivalent, these edge tests insert real
    // task rows first: `parent_edges` has `REFERENCES tasks(id)` and
    // `PRAGMA foreign_keys = ON` is set on every connection (LLD-2
    // §Schema), so an edge naming a task id that was never `put_task`'d
    // would fail the foreign-key constraint rather than silently
    // succeeding the way the in-memory fake's `HashMap` does.

    #[test]
    fn add_parent_edge_then_list_parent_edges_returns_it() {
        let store = SqliteStore::open_in_memory().unwrap();
        let parent = sample_task("task", TaskStatus::Incomplete);
        let child = sample_task("task", TaskStatus::Incomplete);

        store
            .transaction(|tx| {
                tx.put_task(&parent)?;
                tx.put_task(&child)?;
                tx.add_parent_edge(parent.id, child.id)
            })
            .unwrap();

        let edges = store
            .transaction(|tx| tx.list_parent_edges(child.id))
            .unwrap();
        assert_eq!(edges, vec![parent.id]);
    }

    #[test]
    fn add_parent_edge_supports_multiple_parents() {
        let store = SqliteStore::open_in_memory().unwrap();
        let parent_a = sample_task("task", TaskStatus::Incomplete);
        let parent_b = sample_task("task", TaskStatus::Incomplete);
        let child = sample_task("task", TaskStatus::Incomplete);

        store
            .transaction(|tx| {
                tx.put_task(&parent_a)?;
                tx.put_task(&parent_b)?;
                tx.put_task(&child)?;
                tx.add_parent_edge(parent_a.id, child.id)?;
                tx.add_parent_edge(parent_b.id, child.id)?;
                Ok(())
            })
            .unwrap();

        let mut edges = store
            .transaction(|tx| tx.list_parent_edges(child.id))
            .unwrap();
        edges.sort_by_key(|&id| Uuid::from(id));
        let mut expected = vec![parent_a.id, parent_b.id];
        expected.sort_by_key(|&id| Uuid::from(id));
        assert_eq!(edges, expected);
    }

    #[test]
    fn add_parent_edge_is_idempotent_on_duplicate() {
        let store = SqliteStore::open_in_memory().unwrap();
        let parent = sample_task("task", TaskStatus::Incomplete);
        let child = sample_task("task", TaskStatus::Incomplete);

        store
            .transaction(|tx| {
                tx.put_task(&parent)?;
                tx.put_task(&child)?;
                tx.add_parent_edge(parent.id, child.id)?;
                tx.add_parent_edge(parent.id, child.id)
            })
            .unwrap();

        let edges = store
            .transaction(|tx| tx.list_parent_edges(child.id))
            .unwrap();
        assert_eq!(edges, vec![parent.id]);
    }

    #[test]
    fn add_parent_edge_referencing_nonexistent_task_fails_foreign_key_check() {
        // Confirms `PRAGMA foreign_keys = ON` is actually enabled per
        // connection (LLD-2 §Testing Strategy's foreign-key smoke test) —
        // a common rusqlite footgun is setting it once but not on every
        // new connection.
        let store = SqliteStore::open_in_memory().unwrap();
        let result = store.transaction(|tx| tx.add_parent_edge(TaskId::new(), TaskId::new()));
        assert!(result.is_err());
    }

    #[test]
    fn put_task_type_then_get_task_types_returns_it() {
        let store = SqliteStore::open_in_memory().unwrap();
        let task_type = goal_type();

        store
            .transaction(|tx| tx.put_task_type(&task_type))
            .unwrap();

        let listed = store.transaction(|tx| tx.get_task_types()).unwrap();
        assert!(listed.contains(&task_type));
    }

    #[test]
    fn transaction_rolls_back_all_writes_when_closure_returns_err() {
        let store = SqliteStore::open_in_memory().unwrap();
        let task = sample_task("task", TaskStatus::Incomplete);

        let result: Result<(), StoreError> = store.transaction(|tx| {
            tx.put_task(&task)?;
            Err(StoreError::Backend("simulated failure".to_owned()))
        });
        assert!(result.is_err());

        let fetched = store.transaction(|tx| tx.get_task(task.id)).unwrap();
        assert_eq!(fetched, None);
    }
}
