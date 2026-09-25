//! [`SqliteStore`]/`SqliteTx`: the `Store`/`StoreTx` implementation (LLD-2
//! §`Store`/`StoreTx` Implementation, §Decision).

use std::cell::RefCell;
use std::path::Path;

use bala_core::{
    Parents, Placement, Store, StoreError, StoreTx, Task, TaskId, TaskType, TreeFilter, User,
    UserId,
};
use rusqlite::Connection;

use crate::{edges, schema, task, types, user};

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
    fn get_user(&mut self, id: UserId) -> Result<Option<User>, StoreError> {
        user::get_user(&self.tx, id)
    }

    fn put_user(&mut self, user: &User) -> Result<(), StoreError> {
        user::put_user(&self.tx, user)
    }

    fn list_users(&mut self) -> Result<Vec<User>, StoreError> {
        user::list_users(&self.tx)
    }

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

    fn replace_parent_edges(
        &mut self,
        child: TaskId,
        parents: &Parents,
        placement: Placement,
    ) -> Result<(), StoreError> {
        edges::replace_parent_edges(&self.tx, child, parents, placement)
    }

    fn swap_child_positions(
        &mut self,
        parent: Option<TaskId>,
        a: TaskId,
        b: TaskId,
    ) -> Result<(), StoreError> {
        edges::swap_child_positions(&self.tx, parent, a, b)
    }

    fn list_child_edges(&mut self, parent: Option<TaskId>) -> Result<Vec<TaskId>, StoreError> {
        edges::list_child_edges(&self.tx, parent)
    }

    fn list_all_child_edges(&mut self) -> Result<Vec<(Option<TaskId>, TaskId)>, StoreError> {
        edges::list_all_child_edges(&self.tx)
    }

    fn get_task_including_deleted(&mut self, id: TaskId) -> Result<Option<Task>, StoreError> {
        task::get_task_including_deleted(&self.tx, id)
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

    // `User`/`UserId` are already brought in via `super::*` (this module's
    // own `use bala_core::{..., User, UserId}`).

    fn sample_task(type_key: &str, status: TaskStatus) -> Task {
        let now = Utc::now();
        Task {
            id: TaskId::new(),
            title: "Title".to_owned(),
            description: None,
            parent_ids: Vec::new(),
            type_key: type_key.to_owned(),
            status,
            // `bala-store` never persists `progress` (`task_from_row`
            // always returns `0.0` — see its doc comment), so a
            // round-tripped task read back via `get_task`/`list_tasks`
            // never carries this fixture's value regardless of `status`.
            // Match that here so equality assertions against the
            // round-tripped result hold without special-casing `progress`.
            progress: 0.0,
            start_date: None,
            due_date: None,
            assignee_id: None,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            completed_at: None,
        }
    }

    fn sample_user(name: &str) -> User {
        User {
            id: UserId::new(),
            name: name.to_owned(),
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
    fn replace_parent_edges_then_list_parent_edges_returns_it() {
        let store = SqliteStore::open_in_memory().unwrap();
        let parent = sample_task("task", TaskStatus::Incomplete);
        let child = sample_task("task", TaskStatus::Incomplete);

        store
            .transaction(|tx| {
                tx.put_task(&parent)?;
                tx.put_task(&child)?;
                tx.replace_parent_edges(child.id, &Parents::Under(vec![parent.id]), Placement::End)
            })
            .unwrap();

        let edges = store
            .transaction(|tx| tx.list_parent_edges(child.id))
            .unwrap();
        assert_eq!(edges, vec![parent.id]);
    }

    fn put_tasks(store: &SqliteStore, n: usize) -> Vec<TaskId> {
        let tasks: Vec<Task> = (0..n)
            .map(|_| sample_task("task", TaskStatus::Incomplete))
            .collect();
        store
            .transaction(|tx| tasks.iter().try_for_each(|t| tx.put_task(t)))
            .unwrap();
        tasks.iter().map(|t| t.id).collect()
    }

    fn null_edge_count(store: &SqliteStore, child: TaskId) -> i64 {
        store
            .conn
            .borrow()
            .query_row(
                "SELECT COUNT(*) FROM parent_edges WHERE child_id = ?1 AND parent_id IS NULL",
                [crate::convert::id_to_blob(child).as_slice()],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[test]
    fn top_level_task_should_have_single_null_parent_edge() {
        let store = SqliteStore::open_in_memory().unwrap();
        let ids = put_tasks(&store, 2);
        let (a, p) = (ids[0], ids[1]);
        for _ in 0..2 {
            store
                .transaction(|tx| tx.replace_parent_edges(a, &Parents::TopLevel, Placement::End))
                .unwrap();
        }
        assert_eq!(null_edge_count(&store, a), 1);
        assert!(
            store
                .transaction(|tx| tx.list_parent_edges(a))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store.transaction(|tx| tx.list_child_edges(None)).unwrap(),
            vec![a]
        );
        // Under a real parent: NULL edge gone; back to top level: one again.
        store
            .transaction(|tx| tx.replace_parent_edges(a, &Parents::Under(vec![p]), Placement::End))
            .unwrap();
        assert_eq!(null_edge_count(&store, a), 0);
        store
            .transaction(|tx| tx.replace_parent_edges(a, &Parents::TopLevel, Placement::End))
            .unwrap();
        assert_eq!(null_edge_count(&store, a), 1);
    }

    #[test]
    fn replace_parent_edges_should_reject_second_null_edge() {
        let store = SqliteStore::open_in_memory().unwrap();
        let a = put_tasks(&store, 1)[0];
        store
            .transaction(|tx| tx.replace_parent_edges(a, &Parents::TopLevel, Placement::End))
            .unwrap();
        // A raw second NULL edge (unreachable through the trait) must be
        // rejected by the partial unique index.
        let err = store
            .conn
            .borrow()
            .execute(
                "INSERT INTO parent_edges (parent_id, child_id, position) VALUES (NULL, ?1, 99)",
                [crate::convert::id_to_blob(a).as_slice()],
            )
            .unwrap_err();
        assert!(err.to_string().contains("UNIQUE"), "{err}");
        assert_eq!(null_edge_count(&store, a), 1);
    }

    #[test]
    fn list_child_edges_none_should_return_roots_in_position_order() {
        let store = SqliteStore::open_in_memory().unwrap();
        let ids = put_tasks(&store, 3);
        // Insertion order differs from the order the ids were created in.
        let order = [ids[2], ids[0], ids[1]];
        store
            .transaction(|tx| {
                order.iter().try_for_each(|&id| {
                    tx.replace_parent_edges(id, &Parents::TopLevel, Placement::End)
                })
            })
            .unwrap();
        assert_eq!(
            store.transaction(|tx| tx.list_child_edges(None)).unwrap(),
            order.to_vec()
        );
    }

    #[test]
    fn replace_parent_edges_should_replace_whole_set_atomically() {
        let store = SqliteStore::open_in_memory().unwrap();
        let a = sample_task("task", TaskStatus::Incomplete);
        let b = sample_task("task", TaskStatus::Incomplete);
        let c = sample_task("task", TaskStatus::Incomplete);
        let child = sample_task("task", TaskStatus::Incomplete);
        store
            .transaction(|tx| {
                for t in [&a, &b, &c, &child] {
                    tx.put_task(t)?;
                }
                tx.replace_parent_edges(child.id, &Parents::Under(vec![a.id, b.id]), Placement::End)
            })
            .unwrap();

        store
            .transaction(|tx| {
                tx.replace_parent_edges(child.id, &Parents::Under(vec![c.id]), Placement::End)
            })
            .unwrap();

        let edges = store
            .transaction(|tx| tx.list_parent_edges(child.id))
            .unwrap();
        assert_eq!(edges, vec![c.id]);
        assert!(
            store
                .transaction(|tx| tx.list_child_edges(Some(a.id)))
                .unwrap()
                .is_empty()
        );

        store
            .transaction(|tx| tx.replace_parent_edges(child.id, &Parents::TopLevel, Placement::End))
            .unwrap();
        let edges = store
            .transaction(|tx| tx.list_parent_edges(child.id))
            .unwrap();
        assert!(edges.is_empty());
    }

    #[test]
    fn replace_parent_edges_should_keep_edges_it_was_asked_to_keep() {
        let store = SqliteStore::open_in_memory().unwrap();
        let a = sample_task("task", TaskStatus::Incomplete);
        let b = sample_task("task", TaskStatus::Incomplete);
        let c = sample_task("task", TaskStatus::Incomplete);
        let child = sample_task("task", TaskStatus::Incomplete);
        store
            .transaction(|tx| {
                for t in [&a, &b, &c, &child] {
                    tx.put_task(t)?;
                }
                tx.replace_parent_edges(child.id, &Parents::Under(vec![a.id, b.id]), Placement::End)
            })
            .unwrap();

        store
            .transaction(|tx| {
                tx.replace_parent_edges(
                    child.id,
                    &Parents::Under(vec![b.id, c.id, b.id]),
                    Placement::End,
                )
            })
            .unwrap();

        let mut edges = store
            .transaction(|tx| tx.list_parent_edges(child.id))
            .unwrap();
        edges.sort_by_key(|&id| Uuid::from(id));
        let mut expected = vec![b.id, c.id];
        expected.sort_by_key(|&id| Uuid::from(id));
        assert_eq!(edges, expected);
        assert_eq!(
            store
                .transaction(|tx| tx.list_child_edges(Some(b.id)))
                .unwrap(),
            vec![child.id]
        );
    }

    #[test]
    fn replace_parent_edges_referencing_nonexistent_task_fails_foreign_key_check() {
        // Confirms `PRAGMA foreign_keys = ON` is actually enabled per
        // connection (LLD-2 §Testing Strategy's foreign-key smoke test) —
        // a common rusqlite footgun is setting it once but not on every
        // new connection.
        let store = SqliteStore::open_in_memory().unwrap();
        let result = store.transaction(|tx| {
            tx.replace_parent_edges(
                TaskId::new(),
                &Parents::Under(vec![TaskId::new()]),
                Placement::End,
            )
        });
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

    #[test]
    fn put_user_and_get_user_round_trip() {
        let store = SqliteStore::open_in_memory().unwrap();
        let user = sample_user("Ada Lovelace");

        store.transaction(|tx| tx.put_user(&user)).unwrap();

        let fetched = store.transaction(|tx| tx.get_user(user.id)).unwrap();
        assert_eq!(fetched, Some(user));
    }

    #[test]
    fn list_users_should_return_all_created_users() {
        let store = SqliteStore::open_in_memory().unwrap();
        let a = sample_user("Ada Lovelace");
        let b = sample_user("Grace Hopper");

        store
            .transaction(|tx| {
                tx.put_user(&a)?;
                tx.put_user(&b)?;
                Ok(())
            })
            .unwrap();

        let mut listed = store.transaction(|tx| tx.list_users()).unwrap();
        listed.sort_by_key(|u| Uuid::from(u.id));
        let mut expected = vec![a, b];
        expected.sort_by_key(|u| Uuid::from(u.id));
        assert_eq!(listed, expected);
    }

    #[test]
    fn put_task_should_persist_and_round_trip_assignee_id() {
        let store = SqliteStore::open_in_memory().unwrap();
        let user = sample_user("Ada Lovelace");
        store.transaction(|tx| tx.put_user(&user)).unwrap();

        let mut task = sample_task("task", TaskStatus::Incomplete);
        task.assignee_id = Some(user.id);

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
    fn list_tasks_should_filter_by_assignee_id() {
        let store = SqliteStore::open_in_memory().unwrap();
        let user = sample_user("Ada Lovelace");
        store.transaction(|tx| tx.put_user(&user)).unwrap();

        let mut assigned = sample_task("task", TaskStatus::Incomplete);
        assigned.assignee_id = Some(user.id);
        let unassigned = sample_task("task", TaskStatus::Incomplete);

        store
            .transaction(|tx| {
                tx.put_task(&assigned)?;
                tx.put_task(&unassigned)?;
                Ok(())
            })
            .unwrap();

        let filter = TreeFilter {
            assignee_id: Some(user.id),
            ..TreeFilter::default()
        };
        let listed = store.transaction(|tx| tx.list_tasks(&filter)).unwrap();
        assert_eq!(listed, vec![assigned]);
    }

    #[test]
    fn put_task_should_update_existing_row_and_bump_updated_at() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut task = sample_task("task", TaskStatus::Incomplete);
        store.transaction(|tx| tx.put_task(&task)).unwrap();

        task.title = "New Title".to_owned();
        task.status = TaskStatus::Complete;
        task.updated_at += chrono::Duration::seconds(60);
        store.transaction(|tx| tx.put_task(&task)).unwrap();

        let fetched = store.transaction(|tx| tx.get_task(task.id)).unwrap();
        assert_eq!(fetched, Some(task));

        let listed = store
            .transaction(|tx| tx.list_tasks(&TreeFilter::default()))
            .unwrap();
        assert_eq!(listed.len(), 1);
    }

    #[test]
    fn put_task_should_clear_optional_fields_to_null_on_update() {
        let store = SqliteStore::open_in_memory().unwrap();
        let user = sample_user("Ada Lovelace");
        store.transaction(|tx| tx.put_user(&user)).unwrap();

        let mut task = sample_task("task", TaskStatus::Incomplete);
        task.description = Some("Some description".to_owned());
        task.start_date = Some(Utc::now().date_naive());
        task.due_date = Some(Utc::now().date_naive());
        task.assignee_id = Some(user.id);
        store.transaction(|tx| tx.put_task(&task)).unwrap();

        task.description = None;
        task.start_date = None;
        task.due_date = None;
        task.assignee_id = None;
        store.transaction(|tx| tx.put_task(&task)).unwrap();

        let fetched = store.transaction(|tx| tx.get_task(task.id)).unwrap();
        assert_eq!(fetched, Some(task));
    }

    #[test]
    fn put_task_should_persist_deleted_at_and_get_task_including_deleted_should_return_it() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut task = sample_task("task", TaskStatus::Incomplete);
        task.deleted_at = Some(Utc::now());

        store.transaction(|tx| tx.put_task(&task)).unwrap();

        let fetched = store
            .transaction(|tx| tx.get_task_including_deleted(task.id))
            .unwrap();
        assert_eq!(fetched, Some(task));
    }

    #[test]
    fn get_task_should_exclude_a_soft_deleted_row() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut task = sample_task("task", TaskStatus::Incomplete);
        task.deleted_at = Some(Utc::now());

        store.transaction(|tx| tx.put_task(&task)).unwrap();

        let fetched = store.transaction(|tx| tx.get_task(task.id)).unwrap();
        assert_eq!(fetched, None);
    }

    #[test]
    fn put_task_should_persist_completed_at_and_get_task_should_return_it() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut task = sample_task("task", TaskStatus::Complete);
        task.completed_at = Some(Utc::now());

        store.transaction(|tx| tx.put_task(&task)).unwrap();

        let fetched = store.transaction(|tx| tx.get_task(task.id)).unwrap();
        assert_eq!(fetched, Some(task));
    }

    #[test]
    fn put_task_should_update_completed_at_on_conflict() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut task = sample_task("task", TaskStatus::Incomplete);
        store.transaction(|tx| tx.put_task(&task)).unwrap();

        task.status = TaskStatus::Complete;
        task.completed_at = Some(Utc::now());
        store.transaction(|tx| tx.put_task(&task)).unwrap();

        let fetched = store.transaction(|tx| tx.get_task(task.id)).unwrap();
        assert_eq!(fetched, Some(task));
    }

    #[test]
    fn list_child_edges_then_replace_parent_edges_reflects_removal() {
        let store = SqliteStore::open_in_memory().unwrap();
        let parent = sample_task("task", TaskStatus::Incomplete);
        let child = sample_task("task", TaskStatus::Incomplete);

        store
            .transaction(|tx| {
                tx.put_task(&parent)?;
                tx.put_task(&child)?;
                tx.replace_parent_edges(child.id, &Parents::Under(vec![parent.id]), Placement::End)
            })
            .unwrap();

        let children = store
            .transaction(|tx| tx.list_child_edges(Some(parent.id)))
            .unwrap();
        assert_eq!(children, vec![child.id]);

        store
            .transaction(|tx| tx.replace_parent_edges(child.id, &Parents::TopLevel, Placement::End))
            .unwrap();

        let children = store
            .transaction(|tx| tx.list_child_edges(Some(parent.id)))
            .unwrap();
        assert!(children.is_empty());
    }

    #[test]
    fn put_task_should_fail_when_assignee_id_references_nonexistent_user() {
        // Confirms the FK added by `migrations/V2__add_users.sql`
        // (`assignee_id BLOB REFERENCES users(id)`) is actually enforced,
        // mirroring `replace_parent_edges_referencing_nonexistent_task_fails_foreign_key_check`'s
        // reasoning: `PRAGMA foreign_keys = ON` is set on every connection
        // (LLD-2 §Schema), so a task naming a user id that was never
        // `put_user`'d fails the foreign-key constraint.
        let store = SqliteStore::open_in_memory().unwrap();
        let mut task = sample_task("task", TaskStatus::Incomplete);
        task.assignee_id = Some(UserId::new());

        let result = store.transaction(|tx| tx.put_task(&task));
        assert!(result.is_err());
    }

    fn append(tx: &mut dyn StoreTx, child: TaskId, parents: &Parents, at: Placement) {
        tx.replace_parent_edges(child, parents, at).unwrap();
    }

    #[test]
    #[allow(clippy::many_single_char_names)]
    fn replace_parent_edges_should_keep_position_of_retained_parents() {
        let store = SqliteStore::open_in_memory().unwrap();
        let v = put_tasks(&store, 5);
        let (p, q, y, child, z) = (v[0], v[1], v[2], v[3], v[4]);
        store
            .transaction(|tx| {
                append(tx, y, &Parents::Under(vec![p]), Placement::End);
                append(tx, child, &Parents::Under(vec![p]), Placement::End);
                append(tx, z, &Parents::Under(vec![p]), Placement::End);
                append(tx, child, &Parents::Under(vec![p, q]), Placement::After(z));
                Ok(())
            })
            .unwrap();

        let under_p = store
            .transaction(|tx| tx.list_child_edges(Some(p)))
            .unwrap();
        let under_q = store
            .transaction(|tx| tx.list_child_edges(Some(q)))
            .unwrap();

        assert_eq!(under_p, [y, child, z]);
        assert_eq!(under_q, [child]);
    }

    #[test]
    #[allow(clippy::many_single_char_names)]
    fn replace_parent_edges_should_place_after_given_sibling() {
        let store = SqliteStore::open_in_memory().unwrap();
        let v = put_tasks(&store, 7);
        let (p, q, a, b, c, child, other) = (v[0], v[1], v[2], v[3], v[4], v[5], v[6]);
        store
            .transaction(|tx| {
                append(tx, a, &Parents::Under(vec![p]), Placement::End);
                append(tx, b, &Parents::Under(vec![p]), Placement::End);
                append(tx, c, &Parents::Under(vec![q]), Placement::End);
                append(tx, child, &Parents::Under(vec![p, q]), Placement::After(a));
                // Sibling not under the parent: falls back to End.
                append(tx, other, &Parents::Under(vec![p]), Placement::After(c));
                Ok(())
            })
            .unwrap();

        let under_p = store
            .transaction(|tx| tx.list_child_edges(Some(p)))
            .unwrap();
        let under_q = store
            .transaction(|tx| tx.list_child_edges(Some(q)))
            .unwrap();

        assert_eq!(under_p, [a, child, b, other]);
        assert_eq!(under_q, [c, child]);
    }

    #[test]
    fn replace_parent_edges_should_place_top_level_after_given_root() {
        let store = SqliteStore::open_in_memory().unwrap();
        let v = put_tasks(&store, 3);
        let (a, b, child) = (v[0], v[1], v[2]);
        store
            .transaction(|tx| {
                append(tx, a, &Parents::TopLevel, Placement::End);
                append(tx, b, &Parents::TopLevel, Placement::End);
                append(tx, child, &Parents::TopLevel, Placement::After(a));
                Ok(())
            })
            .unwrap();

        let roots = store.transaction(|tx| tx.list_child_edges(None)).unwrap();

        assert_eq!(roots, [a, child, b]);
    }

    #[test]
    #[allow(clippy::many_single_char_names)]
    fn list_all_child_edges_should_return_every_parent_in_position_order() {
        let store = SqliteStore::open_in_memory().unwrap();
        let v = put_tasks(&store, 4);
        let (p, a, b, r) = (v[0], v[1], v[2], v[3]);
        store
            .transaction(|tx| {
                append(tx, p, &Parents::TopLevel, Placement::End);
                append(tx, a, &Parents::Under(vec![p]), Placement::End);
                append(tx, b, &Parents::Under(vec![p]), Placement::End);
                append(tx, r, &Parents::TopLevel, Placement::End);
                Ok(())
            })
            .unwrap();

        let all = store.transaction(|tx| tx.list_all_child_edges()).unwrap();
        let of = |parent| -> Vec<TaskId> {
            all.iter()
                .filter(|(p, _)| *p == parent)
                .map(|&(_, c)| c)
                .collect()
        };

        assert_eq!(of(None), [p, r]);
        assert_eq!(of(Some(p)), [a, b]);
    }

    #[test]
    #[allow(clippy::many_single_char_names)]
    fn swap_child_positions_should_swap_only_the_given_parent() {
        let store = SqliteStore::open_in_memory().unwrap();
        let v = put_tasks(&store, 5);
        let (p, q, a, b, c) = (v[0], v[1], v[2], v[3], v[4]);
        store
            .transaction(|tx| {
                append(tx, a, &Parents::Under(vec![p, q]), Placement::End);
                append(tx, b, &Parents::Under(vec![p, q]), Placement::End);
                append(tx, c, &Parents::Under(vec![p]), Placement::End);
                tx.swap_child_positions(Some(p), a, c)
            })
            .unwrap();

        let under_p = store
            .transaction(|tx| tx.list_child_edges(Some(p)))
            .unwrap();
        let under_q = store
            .transaction(|tx| tx.list_child_edges(Some(q)))
            .unwrap();

        assert_eq!(under_p, [c, b, a]);
        assert_eq!(under_q, [a, b]);
    }

    #[test]
    fn swap_child_positions_should_ignore_task_not_under_parent() {
        let store = SqliteStore::open_in_memory().unwrap();
        let v = put_tasks(&store, 5);
        let (p, a, b) = (v[0], v[1], v[2]);
        store
            .transaction(|tx| {
                append(tx, a, &Parents::Under(vec![p]), Placement::End);
                tx.swap_child_positions(Some(p), a, b)
            })
            .unwrap();

        let under_p = store
            .transaction(|tx| tx.list_child_edges(Some(p)))
            .unwrap();

        assert_eq!(under_p, [a]);
    }
}
