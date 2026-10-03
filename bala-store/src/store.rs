//! [`SqliteStore`]/`SqliteTx`: the `Store`/`StoreTx` implementation (LLD-2
//! §`Store`/`StoreTx` Implementation, §Decision).

use std::cell::RefCell;
use std::path::Path;

use bala_core::{
    Dependency, DependencyType, Placement, Store, StoreError, StoreTx, Task, TaskId, TaskType,
    TreeFilter, User, UserId,
};
use refinery::Runner;
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
    /// Returns `Err` if the file can't be opened, if migrations fail, or if
    /// any stored row violates a foreign key (`PRAGMA foreign_key_check`
    /// reports at least one violation).
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path).map_err(task::sqlite_err)?;
        Self::init(conn, true, &schema::runner())
    }

    /// In-memory (`:memory:`) store — same migrations, no file. Intended
    /// for tests.
    ///
    /// # Errors
    ///
    /// Returns `Err` if migrations fail, or if any stored row violates a
    /// foreign key (`PRAGMA foreign_key_check` reports at least one
    /// violation).
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory().map_err(task::sqlite_err)?;
        Self::init(conn, false, &schema::runner())
    }

    /// Shared setup for both constructors: foreign keys off, WAL if
    /// requested, `migrations` (the constructors pass [`schema::runner`],
    /// whose baseline also seeds the default task type — see
    /// `migrations/V1__init.sql`), then foreign keys back on for every
    /// transaction the store runs afterwards, then a foreign-key check of
    /// every stored row.
    fn init(mut conn: Connection, wal: bool, migrations: &Runner) -> Result<Self, StoreError> {
        // Foreign keys must be off while migrations run: a table-rebuild
        // migration (create-copy-drop-rename of a referenced table such as
        // `tasks`) drops a table that other tables reference, which fails
        // while any referencing row exists. Setting OFF explicitly is
        // required because rusqlite's bundled SQLite is compiled with foreign
        // keys on by default, and it can't live in the migration SQL because
        // the pragma is a no-op inside a transaction and refinery wraps each
        // migration in one.
        conn.pragma_update(None, "foreign_keys", "OFF")
            .map_err(task::sqlite_err)?;
        if wal {
            conn.pragma_update(None, "journal_mode", "WAL")
                .map_err(task::sqlite_err)?;
        }
        migrations
            .run(&mut conn)
            .map_err(|err| migration_err(&err))?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(task::sqlite_err)?;
        check_foreign_keys(&conn)?;
        Ok(Self {
            conn: RefCell::new(conn),
        })
    }
}

/// Maps a refinery error to [`StoreError::Backend`].
///
/// An applied migration whose text differs from this build's means the
/// database was created from an earlier version of the schema baseline,
/// which is rewritten in place until the first release (LLD-2 §Schema,
/// Migration policy). Such a database can't be migrated, so the message
/// says how to recover. A migration missing from this build gets no such
/// hint: that is an older build opening a newer database, where deleting
/// it would be the wrong advice.
fn migration_err(err: &refinery::Error) -> StoreError {
    let message = match err.kind() {
        refinery::error::Kind::DivergentVersion(..) => format!(
            "{err}: the database was created from an earlier schema baseline; \
             delete the database file and a new one is created on next open"
        ),
        _ => err.to_string(),
    };
    StoreError::Backend(message)
}

/// Fails if `PRAGMA foreign_key_check` reports any violation.
///
/// Migrations run with foreign keys off, and SQLite doesn't re-validate
/// existing rows when they are turned back on, so a dangling reference
/// (from a migration or from a file written with foreign keys off) would
/// otherwise go unnoticed until some later write trips over it. The error
/// names the violation count and the first violation's table, rowid (absent
/// for a `WITHOUT ROWID` table) and referenced table.
fn check_foreign_keys(conn: &Connection) -> Result<(), StoreError> {
    let mut stmt = conn
        .prepare("PRAGMA foreign_key_check")
        .map_err(task::sqlite_err)?;
    let mut violations = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(task::sqlite_err)?;
    // Only the first violation is reported, so the rest are counted, not kept.
    let Some(first) = violations.next() else {
        return Ok(());
    };
    let (table, rowid, parent) = first.map_err(task::sqlite_err)?;
    let mut count = 1_usize;
    for violation in violations {
        violation.map_err(task::sqlite_err)?;
        count += 1;
    }
    let row = rowid.map_or_else(|| "a row".to_owned(), |rowid| format!("rowid {rowid}"));
    Err(StoreError::Backend(format!(
        "database has {count} foreign-key violation(s); first: {table} {row} references a missing {parent} row"
    )))
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

    fn get_parent_edge(&mut self, id: TaskId) -> Result<Option<TaskId>, StoreError> {
        edges::get_parent_edge(&self.tx, id)
    }

    fn set_parent_edge(
        &mut self,
        child: TaskId,
        parent: Option<TaskId>,
        placement: Placement,
    ) -> Result<(), StoreError> {
        edges::set_parent_edge(&self.tx, child, parent, placement)
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

    fn list_dependency_edges(&mut self, id: TaskId) -> Result<Vec<Dependency>, StoreError> {
        edges::list_dependency_edges(&self.tx, id)
    }

    fn list_successor_edges(&mut self, id: TaskId) -> Result<Vec<TaskId>, StoreError> {
        edges::list_successor_edges(&self.tx, id)
    }

    fn add_dependency_edge(
        &mut self,
        predecessor: TaskId,
        successor: TaskId,
        dep_type: DependencyType,
    ) -> Result<(), StoreError> {
        edges::add_dependency_edge(&self.tx, predecessor, successor, dep_type)
    }

    fn remove_dependency_edge(
        &mut self,
        predecessor: TaskId,
        successor: TaskId,
    ) -> Result<(), StoreError> {
        edges::remove_dependency_edge(&self.tx, predecessor, successor)
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
    use refinery::Migration;
    use uuid::Uuid;

    // `User`/`UserId` are already brought in via `super::*` (this module's
    // own `use bala_core::{..., User, UserId}`).

    fn sample_task(type_key: &str, status: TaskStatus) -> Task {
        let now = Utc::now();
        Task {
            id: TaskId::new(),
            title: "Title".to_owned(),
            description: None,
            parent_id: None,
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
            duration_days: None,
            dates_fixed: false,
            assignee_id: None,
            depends_on: Vec::new(),
            out_of_sync: false,
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
    fn put_task_then_get_task_round_trips_duration_and_dates_fixed() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut task = sample_task("task", TaskStatus::Incomplete);
        task.start_date = chrono::NaiveDate::from_ymd_opt(2026, 1, 1);
        task.duration_days = Some(3);
        task.dates_fixed = true;
        let mut milestone = sample_task("task", TaskStatus::Incomplete);
        milestone.duration_days = Some(0);

        store
            .transaction(|tx| {
                tx.put_task(&task)?;
                tx.put_task(&milestone)?;
                Ok(())
            })
            .unwrap();

        let fetched = store.transaction(|tx| tx.get_task(task.id)).unwrap();
        assert_eq!(fetched, Some(task));
        let fetched = store.transaction(|tx| tx.get_task(milestone.id)).unwrap();
        assert_eq!(fetched, Some(milestone));
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
    fn get_parent_edge_for_task_with_no_parent_returns_none() {
        let store = SqliteStore::open_in_memory().unwrap();
        let edge = store
            .transaction(|tx| tx.get_parent_edge(TaskId::new()))
            .unwrap();
        assert_eq!(edge, None);
    }

    // Unlike the `InMemoryStore` equivalent, these edge tests insert real
    // task rows first: `parent_edges` has `REFERENCES tasks(id)` and
    // `PRAGMA foreign_keys = ON` is set on every connection (LLD-2
    // §Schema), so an edge naming a task id that was never `put_task`'d
    // would fail the foreign-key constraint rather than silently
    // succeeding the way the in-memory fake's `HashMap` does.

    #[test]
    fn set_parent_edge_then_get_parent_edge_returns_it() {
        let store = SqliteStore::open_in_memory().unwrap();
        let parent = sample_task("task", TaskStatus::Incomplete);
        let child = sample_task("task", TaskStatus::Incomplete);

        store
            .transaction(|tx| {
                tx.put_task(&parent)?;
                tx.put_task(&child)?;
                tx.set_parent_edge(child.id, Some(parent.id), Placement::End)
            })
            .unwrap();

        let edge = store
            .transaction(|tx| tx.get_parent_edge(child.id))
            .unwrap();
        assert_eq!(edge, Some(parent.id));
    }

    #[test]
    #[allow(clippy::many_single_char_names)]
    fn set_parent_edge_should_replace_the_previous_parent() {
        let store = SqliteStore::open_in_memory().unwrap();
        let v = put_tasks(&store, 6);
        let (p, q, a, child, b, c) = (v[0], v[1], v[2], v[3], v[4], v[5]);
        store
            .transaction(|tx| {
                append(tx, a, Some(p), Placement::End);
                append(tx, child, Some(p), Placement::End);
                append(tx, b, Some(p), Placement::End);
                append(tx, c, Some(q), Placement::End);
                Ok(())
            })
            .unwrap();

        store
            .transaction(|tx| tx.set_parent_edge(child, Some(q), Placement::End))
            .unwrap();

        // One edge, to the new parent: the old one is gone, the former
        // siblings keep their order, and the child lands at the end.
        let children = |parent| store.transaction(|tx| tx.list_child_edges(parent)).unwrap();
        let parent_of = |id| store.transaction(|tx| tx.get_parent_edge(id)).unwrap();
        assert_eq!(parent_of(child), Some(q));
        assert_eq!(edge_count(&store, child), 1);
        assert_eq!(children(Some(p)), [a, b]);
        assert_eq!(children(Some(q)), [c, child]);

        // Moving to the top level replaces the real edge with the NULL one,
        // and moving back under a parent replaces the NULL one.
        store
            .transaction(|tx| tx.set_parent_edge(child, None, Placement::End))
            .unwrap();
        assert_eq!(parent_of(child), None);
        assert_eq!(edge_count(&store, child), 1);
        assert_eq!(children(Some(q)), [c]);
        assert_eq!(children(None), [child]);
        store
            .transaction(|tx| tx.set_parent_edge(child, Some(p), Placement::After(a)))
            .unwrap();
        assert_eq!(parent_of(child), Some(p));
        assert_eq!(edge_count(&store, child), 1);
        assert_eq!(children(None), []);
        assert_eq!(children(Some(p)), [a, child, b]);
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

    /// How many `parent_edges` rows `child` has, NULL-parent or real.
    fn edge_count(store: &SqliteStore, child: TaskId) -> i64 {
        store
            .conn
            .borrow()
            .query_row(
                "SELECT COUNT(*) FROM parent_edges WHERE child_id = ?1",
                [crate::convert::id_to_blob(child).as_slice()],
                |row| row.get(0),
            )
            .unwrap()
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
                .transaction(|tx| tx.set_parent_edge(a, None, Placement::End))
                .unwrap();
        }
        assert_eq!(null_edge_count(&store, a), 1);
        assert_eq!(store.transaction(|tx| tx.get_parent_edge(a)).unwrap(), None);
        assert_eq!(
            store.transaction(|tx| tx.list_child_edges(None)).unwrap(),
            vec![a]
        );
        // Under a real parent: NULL edge gone; back to top level: one again.
        store
            .transaction(|tx| tx.set_parent_edge(a, Some(p), Placement::End))
            .unwrap();
        assert_eq!(null_edge_count(&store, a), 0);
        store
            .transaction(|tx| tx.set_parent_edge(a, None, Placement::End))
            .unwrap();
        assert_eq!(null_edge_count(&store, a), 1);
    }

    /// Inserts a `parent_edges` row with raw SQL, bypassing the trait.
    fn insert_raw_edge(
        store: &SqliteStore,
        parent: Option<TaskId>,
        child: TaskId,
    ) -> rusqlite::Result<usize> {
        let parent_blob = parent.map(crate::convert::id_to_blob);
        store.conn.borrow().execute(
            "INSERT INTO parent_edges (parent_id, child_id, position) VALUES (?1, ?2, 99)",
            rusqlite::params![
                parent_blob.as_ref().map(<[u8; 16]>::as_slice),
                crate::convert::id_to_blob(child).as_slice()
            ],
        )
    }

    #[test]
    fn parent_edges_should_reject_a_second_edge_for_a_child() {
        let store = SqliteStore::open_in_memory().unwrap();
        let ids = put_tasks(&store, 4);
        let (p, q, nested, top) = (ids[0], ids[1], ids[2], ids[3]);
        store
            .transaction(|tx| {
                tx.set_parent_edge(nested, Some(p), Placement::End)?;
                tx.set_parent_edge(top, None, Placement::End)
            })
            .unwrap();

        // A second row for a child that already has one (unreachable
        // through the trait) is rejected by the unique index on `child_id`,
        // whatever parent either row names.
        for (child, second_parent) in [
            (nested, Some(q)),
            (nested, Some(p)),
            (nested, None),
            (top, Some(p)),
            (top, None),
        ] {
            let err = insert_raw_edge(&store, second_parent, child).unwrap_err();
            assert_eq!(
                err.sqlite_error_code(),
                Some(rusqlite::ErrorCode::ConstraintViolation),
                "second parent {second_parent:?}: {err}"
            );
            assert!(err.to_string().contains("UNIQUE"), "{err}");
            assert_eq!(edge_count(&store, child), 1);
        }
        let parent_of = |id| store.transaction(|tx| tx.get_parent_edge(id)).unwrap();
        assert_eq!(parent_of(nested), Some(p));
        assert_eq!(parent_of(top), None);
    }

    #[test]
    fn every_task_should_have_exactly_one_parent_edge_row() {
        let store = SqliteStore::open_in_memory().unwrap();
        let ids = put_tasks(&store, 4);
        let (p, q, top, moved) = (ids[0], ids[1], ids[2], ids[3]);
        // Created at the top level or under a parent, then moved to another
        // parent, to the top level and back, and to the parent it already
        // has.
        let steps = [
            (p, None),
            (q, None),
            (top, None),
            (moved, Some(p)),
            (moved, Some(q)),
            (moved, None),
            (moved, None),
            (moved, Some(p)),
            (moved, Some(p)),
            (top, Some(q)),
            (top, Some(q)),
        ];
        store
            .transaction(|tx| {
                steps.iter().try_for_each(|&(child, parent)| {
                    tx.set_parent_edge(child, parent, Placement::End)
                })
            })
            .unwrap();

        let conn = store.conn.borrow();
        let mut stmt = conn
            .prepare("SELECT child_id, COUNT(*) FROM parent_edges GROUP BY child_id")
            .unwrap();
        let mut counts: Vec<(Vec<u8>, i64)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        counts.sort();
        let mut expected: Vec<(Vec<u8>, i64)> = ids
            .iter()
            .map(|&id| (crate::convert::id_to_blob(id).to_vec(), 1))
            .collect();
        expected.sort();
        assert_eq!(counts, expected);
    }

    #[test]
    fn list_child_edges_none_should_return_roots_in_position_order() {
        let store = SqliteStore::open_in_memory().unwrap();
        let ids = put_tasks(&store, 3);
        // Insertion order differs from the order the ids were created in.
        let order = [ids[2], ids[0], ids[1]];
        store
            .transaction(|tx| {
                order
                    .iter()
                    .try_for_each(|&id| tx.set_parent_edge(id, None, Placement::End))
            })
            .unwrap();
        assert_eq!(
            store.transaction(|tx| tx.list_child_edges(None)).unwrap(),
            order.to_vec()
        );
    }

    #[test]
    fn set_parent_edge_referencing_nonexistent_task_fails_foreign_key_check() {
        // Confirms `PRAGMA foreign_keys = ON` is actually enabled per
        // connection (LLD-2 §Testing Strategy's foreign-key smoke test) —
        // a common rusqlite footgun is setting it once but not on every
        // new connection.
        let store = SqliteStore::open_in_memory().unwrap();
        let result = store.transaction(|tx| {
            tx.set_parent_edge(TaskId::new(), Some(TaskId::new()), Placement::End)
        });
        assert!(result.is_err());
    }

    #[test]
    fn open_should_run_a_rebuild_migration_over_referenced_rows() {
        // A child table references a parent table the same way
        // `parent_edges` references `tasks`; the second migration rebuilds
        // the parent (create-copy-drop-rename), which only succeeds with
        // foreign keys off while migrations run.
        const BASE: &str = "
            CREATE TABLE parents (id BLOB PRIMARY KEY, name TEXT NOT NULL);
            CREATE TABLE children (
                parent_id BLOB REFERENCES parents(id),
                child_id  BLOB NOT NULL REFERENCES parents(id)
            );";
        const REBUILD: &str = "
            CREATE TABLE parents_rebuilt (id BLOB PRIMARY KEY, name TEXT NOT NULL, note TEXT);
            INSERT INTO parents_rebuilt (id, name) SELECT id, name FROM parents;
            DROP TABLE parents;
            ALTER TABLE parents_rebuilt RENAME TO parents;";
        let base = || Migration::unapplied("V1__base", BASE).unwrap();
        let rebuild = Migration::unapplied("V2__rebuild_parents", REBUILD).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rebuild.db");
        drop(
            SqliteStore::init(
                Connection::open(&path).unwrap(),
                true,
                &Runner::new(&[base()]),
            )
            .unwrap(),
        );
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "INSERT INTO parents (id, name) VALUES (x'01', 'parent'), (x'02', 'child');
                 INSERT INTO children (parent_id, child_id) VALUES (x'01', x'02');",
            )
            .unwrap();
        }

        let store = SqliteStore::init(
            Connection::open(&path).unwrap(),
            true,
            &Runner::new(&[base(), rebuild]),
        )
        .unwrap();

        let conn = store.conn.borrow();
        let parents: i64 = conn
            .query_row("SELECT COUNT(*) FROM parents", [], |row| row.get(0))
            .unwrap();
        let edge: (Vec<u8>, Vec<u8>) = conn
            .query_row(
                "SELECT c.parent_id, c.child_id FROM children c
                 JOIN parents p ON p.id = c.parent_id
                 JOIN parents q ON q.id = c.child_id",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(parents, 2);
        assert_eq!(edge, (vec![1], vec![2]));
    }

    #[test]
    fn open_should_fail_when_stored_rows_violate_a_foreign_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dangling.db");
        let blob = crate::convert::id_to_blob;
        {
            let mut conn = Connection::open(&path).unwrap();
            schema::runner().run(&mut conn).unwrap();
            conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
            conn.execute(
                "INSERT INTO parent_edges (parent_id, child_id, position) VALUES (?1, ?2, 0)",
                rusqlite::params![
                    blob(TaskId::new()).as_slice(),
                    blob(TaskId::new()).as_slice()
                ],
            )
            .unwrap();
        }

        let err = SqliteStore::open(&path).unwrap_err();

        let StoreError::Backend(message) = err;
        assert!(message.contains("parent_edges"), "message: {message}");
        // Both the dangling parent and the dangling child are violations.
        assert!(
            message.contains("2 foreign-key violation(s)"),
            "message: {message}"
        );
    }

    #[test]
    fn open_should_suggest_a_reset_when_the_applied_baseline_differs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old-baseline.db");
        let old_baseline =
            Migration::unapplied("V1__init", "CREATE TABLE t (id INTEGER);").unwrap();
        drop(
            SqliteStore::init(
                Connection::open(&path).unwrap(),
                true,
                &Runner::new(&[old_baseline]),
            )
            .unwrap(),
        );

        let err = SqliteStore::open(&path).unwrap_err();

        let StoreError::Backend(message) = err;
        assert!(message.contains("is different than"), "message: {message}");
        assert!(
            message.contains("delete the database file"),
            "message: {message}"
        );
    }

    #[test]
    fn open_should_leave_foreign_keys_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(&dir.path().join("bala.db")).unwrap();

        let enabled: i64 = store
            .conn
            .borrow()
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();

        assert_eq!(enabled, 1);
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
    fn list_child_edges_then_set_parent_edge_reflects_removal() {
        let store = SqliteStore::open_in_memory().unwrap();
        let parent = sample_task("task", TaskStatus::Incomplete);
        let child = sample_task("task", TaskStatus::Incomplete);

        store
            .transaction(|tx| {
                tx.put_task(&parent)?;
                tx.put_task(&child)?;
                tx.set_parent_edge(child.id, Some(parent.id), Placement::End)
            })
            .unwrap();

        let children = store
            .transaction(|tx| tx.list_child_edges(Some(parent.id)))
            .unwrap();
        assert_eq!(children, vec![child.id]);

        store
            .transaction(|tx| tx.set_parent_edge(child.id, None, Placement::End))
            .unwrap();

        let children = store
            .transaction(|tx| tx.list_child_edges(Some(parent.id)))
            .unwrap();
        assert_eq!(children, []);
    }

    #[test]
    fn put_task_should_fail_when_assignee_id_references_nonexistent_user() {
        // Confirms the FK `tasks.assignee_id BLOB REFERENCES users(id)`
        // (`migrations/V1__init.sql`) is actually enforced,
        // mirroring `set_parent_edge_referencing_nonexistent_task_fails_foreign_key_check`'s
        // reasoning: `PRAGMA foreign_keys = ON` is set on every connection
        // (LLD-2 §Schema), so a task naming a user id that was never
        // `put_user`'d fails the foreign-key constraint.
        let store = SqliteStore::open_in_memory().unwrap();
        let mut task = sample_task("task", TaskStatus::Incomplete);
        task.assignee_id = Some(UserId::new());

        let result = store.transaction(|tx| tx.put_task(&task));
        assert!(result.is_err());
    }

    fn append(tx: &mut dyn StoreTx, child: TaskId, parent: Option<TaskId>, at: Placement) {
        tx.set_parent_edge(child, parent, at).unwrap();
    }

    #[test]
    #[allow(clippy::many_single_char_names)]
    fn set_parent_edge_should_keep_position_when_parent_is_unchanged() {
        let store = SqliteStore::open_in_memory().unwrap();
        let v = put_tasks(&store, 4);
        let (p, y, child, z) = (v[0], v[1], v[2], v[3]);
        store
            .transaction(|tx| {
                append(tx, y, Some(p), Placement::End);
                append(tx, child, Some(p), Placement::End);
                append(tx, z, Some(p), Placement::End);
                append(tx, child, Some(p), Placement::After(z));
                append(tx, child, Some(p), Placement::End);
                Ok(())
            })
            .unwrap();

        let under_p = store
            .transaction(|tx| tx.list_child_edges(Some(p)))
            .unwrap();

        assert_eq!(under_p, [y, child, z]);
    }

    #[test]
    #[allow(clippy::many_single_char_names)]
    fn set_parent_edge_should_place_after_given_sibling() {
        let store = SqliteStore::open_in_memory().unwrap();
        let v = put_tasks(&store, 7);
        let (p, q, a, b, c, child, other) = (v[0], v[1], v[2], v[3], v[4], v[5], v[6]);
        store
            .transaction(|tx| {
                append(tx, a, Some(p), Placement::End);
                append(tx, b, Some(p), Placement::End);
                append(tx, c, Some(q), Placement::End);
                append(tx, child, Some(p), Placement::After(a));
                // Sibling not under the parent: falls back to End.
                append(tx, other, Some(p), Placement::After(c));
                Ok(())
            })
            .unwrap();

        let under_p = store
            .transaction(|tx| tx.list_child_edges(Some(p)))
            .unwrap();

        assert_eq!(under_p, [a, child, b, other]);
    }

    #[test]
    fn set_parent_edge_should_place_top_level_after_given_root() {
        let store = SqliteStore::open_in_memory().unwrap();
        let v = put_tasks(&store, 3);
        let (a, b, child) = (v[0], v[1], v[2]);
        store
            .transaction(|tx| {
                append(tx, a, None, Placement::End);
                append(tx, b, None, Placement::End);
                append(tx, child, None, Placement::After(a));
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
                append(tx, p, None, Placement::End);
                append(tx, a, Some(p), Placement::End);
                append(tx, b, Some(p), Placement::End);
                append(tx, r, None, Placement::End);
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
    fn swap_child_positions_should_swap_the_two_children() {
        let store = SqliteStore::open_in_memory().unwrap();
        let v = put_tasks(&store, 4);
        let (p, a, b, c) = (v[0], v[1], v[2], v[3]);
        store
            .transaction(|tx| {
                append(tx, a, Some(p), Placement::End);
                append(tx, b, Some(p), Placement::End);
                append(tx, c, Some(p), Placement::End);
                tx.swap_child_positions(Some(p), a, c)
            })
            .unwrap();

        let under_p = store
            .transaction(|tx| tx.list_child_edges(Some(p)))
            .unwrap();

        assert_eq!(under_p, [c, b, a]);
    }

    #[test]
    fn swap_child_positions_should_ignore_task_not_under_parent() {
        let store = SqliteStore::open_in_memory().unwrap();
        let v = put_tasks(&store, 5);
        let (p, a, b) = (v[0], v[1], v[2]);
        store
            .transaction(|tx| {
                append(tx, a, Some(p), Placement::End);
                tx.swap_child_positions(Some(p), a, b)
            })
            .unwrap();

        let under_p = store
            .transaction(|tx| tx.list_child_edges(Some(p)))
            .unwrap();

        assert_eq!(under_p, [a]);
    }

    fn dep(predecessor_id: TaskId, dep_type: DependencyType) -> Dependency {
        Dependency {
            predecessor_id,
            dep_type,
        }
    }

    #[test]
    fn add_dependency_edge_then_list_dependency_edges_returns_it() {
        let store = SqliteStore::open_in_memory().unwrap();
        let v = put_tasks(&store, 2);
        let (pred, succ) = (v[0], v[1]);

        store
            .transaction(|tx| tx.add_dependency_edge(pred, succ, DependencyType::StartToStart))
            .unwrap();

        let edges = store
            .transaction(|tx| tx.list_dependency_edges(succ))
            .unwrap();
        assert_eq!(edges, [dep(pred, DependencyType::StartToStart)]);
        // The edge is directed: the predecessor does not depend on anything.
        let reverse = store
            .transaction(|tx| tx.list_dependency_edges(pred))
            .unwrap();
        assert_eq!(reverse, []);
    }

    #[test]
    fn add_dependency_edge_should_replace_type_on_existing_pair() {
        let store = SqliteStore::open_in_memory().unwrap();
        let v = put_tasks(&store, 4);
        let (a, b, succ, other) = (v[0], v[1], v[2], v[3]);
        store
            .transaction(|tx| {
                tx.add_dependency_edge(a, succ, DependencyType::FinishToStart)?;
                tx.add_dependency_edge(b, succ, DependencyType::FinishToStart)?;
                tx.add_dependency_edge(a, other, DependencyType::FinishToStart)?;
                tx.add_dependency_edge(a, succ, DependencyType::FinishToFinish)
            })
            .unwrap();

        let edges = store
            .transaction(|tx| tx.list_dependency_edges(succ))
            .unwrap();
        let successors = store.transaction(|tx| tx.list_successor_edges(a)).unwrap();

        // One edge per pair, and the replaced edge keeps its place in both lists.
        assert_eq!(
            edges,
            [
                dep(a, DependencyType::FinishToFinish),
                dep(b, DependencyType::FinishToStart)
            ]
        );
        assert_eq!(successors, [succ, other]);
    }

    #[test]
    fn list_dependency_edges_should_return_predecessors_in_insertion_order() {
        let store = SqliteStore::open_in_memory().unwrap();
        let v = put_tasks(&store, 4);
        let (a, b, c, succ) = (v[0], v[1], v[2], v[3]);
        // Insertion order deliberately differs from the order the ids were made in.
        store
            .transaction(|tx| {
                tx.add_dependency_edge(c, succ, DependencyType::StartToFinish)?;
                tx.add_dependency_edge(a, succ, DependencyType::FinishToStart)?;
                tx.add_dependency_edge(b, succ, DependencyType::StartToStart)
            })
            .unwrap();

        let edges = store
            .transaction(|tx| tx.list_dependency_edges(succ))
            .unwrap();

        assert_eq!(
            edges,
            [
                dep(c, DependencyType::StartToFinish),
                dep(a, DependencyType::FinishToStart),
                dep(b, DependencyType::StartToStart)
            ]
        );
    }

    #[test]
    fn list_successor_edges_should_return_all_successors_of_multi_successor_predecessor() {
        let store = SqliteStore::open_in_memory().unwrap();
        let v = put_tasks(&store, 5);
        let (pred, a, b, c, unrelated) = (v[0], v[1], v[2], v[3], v[4]);
        store
            .transaction(|tx| {
                tx.add_dependency_edge(pred, c, DependencyType::FinishToStart)?;
                tx.add_dependency_edge(unrelated, a, DependencyType::FinishToStart)?;
                tx.add_dependency_edge(pred, a, DependencyType::StartToStart)?;
                tx.add_dependency_edge(pred, b, DependencyType::FinishToFinish)
            })
            .unwrap();

        let successors = store
            .transaction(|tx| tx.list_successor_edges(pred))
            .unwrap();
        let of_leaf = store.transaction(|tx| tx.list_successor_edges(c)).unwrap();

        assert_eq!(successors, [c, a, b]);
        assert_eq!(of_leaf, []);
    }

    #[test]
    fn remove_dependency_edge_should_remove_only_that_pair() {
        let store = SqliteStore::open_in_memory().unwrap();
        let v = put_tasks(&store, 4);
        let (a, b, succ, other) = (v[0], v[1], v[2], v[3]);
        store
            .transaction(|tx| {
                tx.add_dependency_edge(a, succ, DependencyType::FinishToStart)?;
                tx.add_dependency_edge(b, succ, DependencyType::StartToStart)?;
                tx.add_dependency_edge(a, other, DependencyType::FinishToFinish)?;
                tx.remove_dependency_edge(a, succ)
            })
            .unwrap();

        let of_succ = store
            .transaction(|tx| tx.list_dependency_edges(succ))
            .unwrap();
        let of_other = store
            .transaction(|tx| tx.list_dependency_edges(other))
            .unwrap();
        let after_a = store.transaction(|tx| tx.list_successor_edges(a)).unwrap();
        let after_b = store.transaction(|tx| tx.list_successor_edges(b)).unwrap();

        assert_eq!(of_succ, [dep(b, DependencyType::StartToStart)]);
        assert_eq!(of_other, [dep(a, DependencyType::FinishToFinish)]);
        assert_eq!(after_a, [other]);
        assert_eq!(after_b, [succ]);
    }

    #[test]
    fn remove_dependency_edge_should_do_nothing_for_a_missing_pair() {
        let store = SqliteStore::open_in_memory().unwrap();
        let v = put_tasks(&store, 3);
        let (pred, succ, stranger) = (v[0], v[1], v[2]);
        store
            .transaction(|tx| {
                tx.add_dependency_edge(pred, succ, DependencyType::FinishToStart)?;
                // Never linked, and the existing pair the wrong way round.
                tx.remove_dependency_edge(stranger, succ)?;
                tx.remove_dependency_edge(succ, pred)
            })
            .unwrap();

        let edges = store
            .transaction(|tx| tx.list_dependency_edges(succ))
            .unwrap();
        let successors = store
            .transaction(|tx| tx.list_successor_edges(pred))
            .unwrap();

        assert_eq!(edges, [dep(pred, DependencyType::FinishToStart)]);
        assert_eq!(successors, [succ]);
    }

    #[test]
    fn add_dependency_edge_referencing_nonexistent_task_fails_foreign_key_check() {
        // `dependency_edges` references `tasks(id)` on both ends, the same
        // way `parent_edges` does, so an edge naming a task id that was never
        // `put_task`'d fails the foreign-key constraint.
        let store = SqliteStore::open_in_memory().unwrap();
        let existing = put_tasks(&store, 1)[0];

        let missing_predecessor = store.transaction(|tx| {
            tx.add_dependency_edge(TaskId::new(), existing, DependencyType::FinishToStart)
        });
        let missing_successor = store.transaction(|tx| {
            tx.add_dependency_edge(existing, TaskId::new(), DependencyType::FinishToStart)
        });

        assert!(missing_predecessor.is_err());
        assert!(missing_successor.is_err());
        assert_eq!(
            store
                .transaction(|tx| tx.list_successor_edges(existing))
                .unwrap(),
            []
        );
    }

    #[test]
    fn get_task_should_fill_depends_on_from_dependency_edges() {
        let store = SqliteStore::open_in_memory().unwrap();
        let a = sample_task("task", TaskStatus::Incomplete);
        let b = sample_task("task", TaskStatus::Incomplete);
        let succ = sample_task("task", TaskStatus::Incomplete);
        store
            .transaction(|tx| {
                tx.put_task(&a)?;
                tx.put_task(&b)?;
                tx.put_task(&succ)?;
                tx.add_dependency_edge(a.id, succ.id, DependencyType::FinishToStart)?;
                tx.add_dependency_edge(b.id, succ.id, DependencyType::StartToStart)
            })
            .unwrap();

        let fetched = store.transaction(|tx| tx.get_task(succ.id)).unwrap();
        let predecessor = store.transaction(|tx| tx.get_task(a.id)).unwrap();

        assert_eq!(
            fetched.unwrap().depends_on,
            [
                dep(a.id, DependencyType::FinishToStart),
                dep(b.id, DependencyType::StartToStart)
            ]
        );
        // The edge is directed: a predecessor does not depend on its successor.
        assert_eq!(predecessor.unwrap().depends_on, []);
    }

    #[test]
    fn get_task_including_deleted_should_fill_depends_on() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut pred = sample_task("task", TaskStatus::Incomplete);
        pred.deleted_at = Some(Utc::now());
        let mut deleted = sample_task("task", TaskStatus::Incomplete);
        deleted.deleted_at = Some(Utc::now());
        let live = sample_task("task", TaskStatus::Incomplete);
        store
            .transaction(|tx| {
                tx.put_task(&pred)?;
                tx.put_task(&deleted)?;
                tx.put_task(&live)?;
                tx.add_dependency_edge(pred.id, deleted.id, DependencyType::FinishToFinish)?;
                tx.add_dependency_edge(deleted.id, live.id, DependencyType::FinishToStart)
            })
            .unwrap();

        let of_deleted = store
            .transaction(|tx| tx.get_task_including_deleted(deleted.id))
            .unwrap();
        let of_live = store
            .transaction(|tx| tx.get_task_including_deleted(live.id))
            .unwrap();

        // A soft-deleted task keeps its dependency edges, at either end.
        assert_eq!(
            of_deleted.unwrap().depends_on,
            [dep(pred.id, DependencyType::FinishToFinish)]
        );
        assert_eq!(
            of_live.unwrap().depends_on,
            [dep(deleted.id, DependencyType::FinishToStart)]
        );
    }

    #[test]
    fn list_tasks_should_fill_depends_on_for_each_task() {
        let store = SqliteStore::open_in_memory().unwrap();
        let a = sample_task("task", TaskStatus::Incomplete);
        let b = sample_task("task", TaskStatus::Incomplete);
        let c = sample_task("task", TaskStatus::Incomplete);
        store
            .transaction(|tx| {
                tx.put_task(&a)?;
                tx.put_task(&b)?;
                tx.put_task(&c)?;
                tx.add_dependency_edge(a.id, b.id, DependencyType::StartToFinish)?;
                tx.add_dependency_edge(b.id, c.id, DependencyType::FinishToStart)?;
                tx.add_dependency_edge(a.id, c.id, DependencyType::StartToStart)
            })
            .unwrap();

        let listed = store
            .transaction(|tx| tx.list_tasks(&TreeFilter::default()))
            .unwrap();

        let depends_on = |id: TaskId| {
            let task = listed.iter().find(|task| task.id == id).unwrap();
            task.depends_on.clone()
        };
        assert_eq!(listed.len(), 3);
        assert_eq!(depends_on(a.id), []);
        assert_eq!(depends_on(b.id), [dep(a.id, DependencyType::StartToFinish)]);
        assert_eq!(
            depends_on(c.id),
            [
                dep(b.id, DependencyType::FinishToStart),
                dep(a.id, DependencyType::StartToStart)
            ]
        );
    }

    #[test]
    fn put_task_should_not_change_dependency_edges() {
        let store = SqliteStore::open_in_memory().unwrap();
        let pred = sample_task("task", TaskStatus::Incomplete);
        let other = sample_task("task", TaskStatus::Incomplete);
        let mut succ = sample_task("task", TaskStatus::Incomplete);
        store
            .transaction(|tx| {
                tx.put_task(&pred)?;
                tx.put_task(&other)?;
                tx.put_task(&succ)?;
                tx.add_dependency_edge(pred.id, succ.id, DependencyType::FinishToStart)
            })
            .unwrap();

        // Names a predecessor the edges don't have and drops the one they do.
        succ.depends_on = vec![dep(other.id, DependencyType::StartToStart)];
        store.transaction(|tx| tx.put_task(&succ)).unwrap();

        let edges = store
            .transaction(|tx| tx.list_dependency_edges(succ.id))
            .unwrap();
        let of_other = store
            .transaction(|tx| tx.list_successor_edges(other.id))
            .unwrap();
        let fetched = store.transaction(|tx| tx.get_task(succ.id)).unwrap();

        assert_eq!(edges, [dep(pred.id, DependencyType::FinishToStart)]);
        assert_eq!(of_other, []);
        assert_eq!(
            fetched.unwrap().depends_on,
            [dep(pred.id, DependencyType::FinishToStart)]
        );
    }
}
