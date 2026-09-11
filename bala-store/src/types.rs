//! `task_types` CRUD.

use bala_core::{StoreError, TaskType};
use rusqlite::{Transaction, params};

use crate::task::sqlite_err;

pub(crate) fn get_task_types(tx: &Transaction) -> Result<Vec<TaskType>, StoreError> {
    let mut stmt = tx
        .prepare("SELECT key, label, color, sort_order FROM task_types ORDER BY sort_order")
        .map_err(sqlite_err)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(TaskType {
                key: row.get(0)?,
                label: row.get(1)?,
                color: row.get(2)?,
                sort_order: row.get(3)?,
            })
        })
        .map_err(sqlite_err)?;

    let mut types = Vec::new();
    for row in rows {
        types.push(row.map_err(sqlite_err)?);
    }
    Ok(types)
}

/// Upserts by `key` (LLD `ON CONFLICT(key) DO UPDATE`).
pub(crate) fn put_task_type(tx: &Transaction, t: &TaskType) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO task_types (key, label, color, sort_order) VALUES (?1, ?2, ?3, ?4)
        ON CONFLICT(key) DO UPDATE SET
            label = excluded.label,
            color = excluded.color,
            sort_order = excluded.sort_order",
        params![t.key, t.label, t.color, t.sort_order],
    )
    .map_err(sqlite_err)?;
    Ok(())
}
