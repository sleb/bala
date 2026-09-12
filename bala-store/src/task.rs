//! `tasks` row <-> [`Task`] mapping and queries.
//!
//! `out_of_sync` is unused by `bala-core`'s current `Task` shape (see crate
//! docs) and is written with a fixed default (`0`) on every `put_task`,
//! never read back into a `Task`, since `Task` has nowhere to put it yet.
//! `assignee_id` (Story 1.1a), `deleted_at` (Story 1.3), and `completed_at`
//! (Story 1.4) *are* read/written.

use bala_core::{StoreError, Task, TaskId, TreeFilter};
use rusqlite::{OptionalExtension, Row, ToSql, Transaction, params};

use crate::convert::{
    blob_to_task_id, blob_to_user_id, date_from_text, date_to_text, id_to_blob, status_from_text,
    status_to_text, timestamp_from_text, timestamp_to_text, user_id_to_blob,
};
use crate::edges;

const SELECT_COLUMNS: &str = "id, title, description, type_key, status, start_date, due_date, \
    assignee_id, created_at, updated_at, completed_at, deleted_at";

/// Builds a [`Task`] from a row of [`SELECT_COLUMNS`], leaving `parent_ids`
/// empty — callers fill it in from `parent_edges` separately (edges are a
/// distinct table, not embedded in the row).
fn task_from_row(row: &Row) -> rusqlite::Result<Result<Task, StoreError>> {
    let id_blob: Vec<u8> = row.get(0)?;
    let title: String = row.get(1)?;
    let description: Option<String> = row.get(2)?;
    let type_key: String = row.get(3)?;
    let status_text: String = row.get(4)?;
    let start_date: Option<String> = row.get(5)?;
    let due_date: Option<String> = row.get(6)?;
    let assignee_id: Option<Vec<u8>> = row.get(7)?;
    let created_at: String = row.get(8)?;
    let updated_at: String = row.get(9)?;
    let completed_at: Option<String> = row.get(10)?;
    let deleted_at: Option<String> = row.get(11)?;

    Ok((|| {
        Ok(Task {
            id: blob_to_task_id(&id_blob)?,
            title,
            description,
            parent_ids: Vec::new(),
            type_key,
            status: status_from_text(&status_text)?,
            start_date: start_date.map(|s| date_from_text(&s)).transpose()?,
            due_date: due_date.map(|s| date_from_text(&s)).transpose()?,
            assignee_id: assignee_id.map(|b| blob_to_user_id(&b)).transpose()?,
            created_at: timestamp_from_text(&created_at)?,
            updated_at: timestamp_from_text(&updated_at)?,
            completed_at: completed_at.map(|s| timestamp_from_text(&s)).transpose()?,
            deleted_at: deleted_at.map(|s| timestamp_from_text(&s)).transpose()?,
        })
    })())
}

/// Shared implementation for [`get_task`] and [`get_task_including_deleted`]:
/// runs `sql` (expected to select [`SELECT_COLUMNS`] and filter on `id`),
/// maps the row, and fills in `parent_ids`.
fn get_task_with_sql(tx: &Transaction, sql: &str, id: TaskId) -> Result<Option<Task>, StoreError> {
    let id_blob = id_to_blob(id);
    let found = tx
        .query_row(sql, params![id_blob.as_slice()], task_from_row)
        .optional()
        .map_err(sqlite_err)?;
    let Some(mut task) = found.transpose()? else {
        return Ok(None);
    };
    task.parent_ids = edges::list_parent_edges(tx, id)?;
    Ok(Some(task))
}

pub(crate) fn get_task(tx: &Transaction, id: TaskId) -> Result<Option<Task>, StoreError> {
    let sql = format!("SELECT {SELECT_COLUMNS} FROM tasks WHERE id = ?1 AND deleted_at IS NULL");
    get_task_with_sql(tx, &sql, id)
}

/// Like [`get_task`], but also returns a soft-deleted row (doesn't filter
/// on `deleted_at IS NULL`).
pub(crate) fn get_task_including_deleted(
    tx: &Transaction,
    id: TaskId,
) -> Result<Option<Task>, StoreError> {
    let sql = format!("SELECT {SELECT_COLUMNS} FROM tasks WHERE id = ?1");
    get_task_with_sql(tx, &sql, id)
}

pub(crate) fn put_task(tx: &Transaction, task: &Task) -> Result<(), StoreError> {
    let id_blob = id_to_blob(task.id);
    let assignee_id_blob = task.assignee_id.map(user_id_to_blob);
    tx.execute(
        "INSERT INTO tasks (
            id, title, description, type_key, status, start_date, due_date,
            assignee_id, out_of_sync, created_at, updated_at, completed_at, deleted_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9, ?10, ?11, ?12)
        ON CONFLICT(id) DO UPDATE SET
            title = excluded.title,
            description = excluded.description,
            type_key = excluded.type_key,
            status = excluded.status,
            start_date = excluded.start_date,
            due_date = excluded.due_date,
            assignee_id = excluded.assignee_id,
            updated_at = excluded.updated_at,
            completed_at = excluded.completed_at,
            deleted_at = excluded.deleted_at",
        params![
            id_blob.as_slice(),
            task.title,
            task.description,
            task.type_key,
            status_to_text(task.status),
            task.start_date.map(date_to_text),
            task.due_date.map(date_to_text),
            assignee_id_blob.as_ref().map(<[u8; 16]>::as_slice),
            timestamp_to_text(task.created_at),
            timestamp_to_text(task.updated_at),
            task.completed_at.map(timestamp_to_text),
            task.deleted_at.map(timestamp_to_text),
        ],
    )
    .map_err(sqlite_err)?;
    Ok(())
}

pub(crate) fn list_tasks(tx: &Transaction, filter: &TreeFilter) -> Result<Vec<Task>, StoreError> {
    let mut sql = format!("SELECT {SELECT_COLUMNS} FROM tasks WHERE 1 = 1");
    let mut owned_params: Vec<Box<dyn ToSql>> = Vec::new();

    if !filter.include_deleted {
        sql.push_str(" AND deleted_at IS NULL");
    }
    if let Some(type_key) = &filter.type_key {
        sql.push_str(" AND type_key = ?");
        owned_params.push(Box::new(type_key.clone()));
    }
    if let Some(status) = filter.status {
        sql.push_str(" AND status = ?");
        owned_params.push(Box::new(status_to_text(status).to_owned()));
    }
    if let Some(assignee_id) = filter.assignee_id {
        sql.push_str(" AND assignee_id = ?");
        owned_params.push(Box::new(user_id_to_blob(assignee_id).to_vec()));
    }

    let mut stmt = tx.prepare(&sql).map_err(sqlite_err)?;
    let param_refs: Vec<&dyn ToSql> = owned_params.iter().map(AsRef::as_ref).collect();
    let rows = stmt
        .query_map(param_refs.as_slice(), task_from_row)
        .map_err(sqlite_err)?;

    let mut tasks = Vec::new();
    for row in rows {
        let mut task = row.map_err(sqlite_err)??;
        task.parent_ids = edges::list_parent_edges(tx, task.id)?;
        tasks.push(task);
    }
    Ok(tasks)
}

/// Maps a `rusqlite::Error` into `bala_core::StoreError` — see this crate's
/// module docs for why `.to_string()` into the existing `Backend` variant is
/// the chosen approach rather than growing a separate error type.
///
/// Takes `err` by value (rather than the reference `clippy::pedantic`
/// suggests) so it can be passed directly as a `map_err` fn pointer at
/// every call site instead of wrapping each one in a closure.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sqlite_err(err: rusqlite::Error) -> StoreError {
    StoreError::Backend(err.to_string())
}
