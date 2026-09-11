//! `parent_edges` queries.
//!
//! `dependency_edges` exists in the schema (LLD-2 §Schema) but has no
//! module of its own yet — it sits unused until the dependency-edge
//! `StoreTx` methods land in a later story/epic that actually needs them.

use bala_core::{StoreError, TaskId};
use rusqlite::{Transaction, params};

use crate::convert::{blob_to_task_id, id_to_blob};
use crate::task::sqlite_err;

pub(crate) fn list_parent_edges(tx: &Transaction, id: TaskId) -> Result<Vec<TaskId>, StoreError> {
    let id_blob = id_to_blob(id);
    let mut stmt = tx
        .prepare("SELECT parent_id FROM parent_edges WHERE child_id = ?1")
        .map_err(sqlite_err)?;
    let rows = stmt
        .query_map(params![id_blob.as_slice()], |row| row.get::<_, Vec<u8>>(0))
        .map_err(sqlite_err)?;

    let mut parent_ids = Vec::new();
    for row in rows {
        let blob = row.map_err(sqlite_err)?;
        parent_ids.push(blob_to_task_id(&blob)?);
    }
    Ok(parent_ids)
}

/// Idempotent: re-adding an already-existing edge is a silent no-op
/// (`INSERT OR IGNORE`), matching the LLD's upsert semantics for edges.
pub(crate) fn add_parent_edge(
    tx: &Transaction,
    parent: TaskId,
    child: TaskId,
) -> Result<(), StoreError> {
    let parent_blob = id_to_blob(parent);
    let child_blob = id_to_blob(child);
    tx.execute(
        "INSERT OR IGNORE INTO parent_edges (parent_id, child_id) VALUES (?1, ?2)",
        params![parent_blob.as_slice(), child_blob.as_slice()],
    )
    .map_err(sqlite_err)?;
    Ok(())
}

pub(crate) fn list_child_edges(tx: &Transaction, id: TaskId) -> Result<Vec<TaskId>, StoreError> {
    let id_blob = id_to_blob(id);
    let mut stmt = tx
        .prepare("SELECT child_id FROM parent_edges WHERE parent_id = ?1")
        .map_err(sqlite_err)?;
    let rows = stmt
        .query_map(params![id_blob.as_slice()], |row| row.get::<_, Vec<u8>>(0))
        .map_err(sqlite_err)?;

    let mut child_ids = Vec::new();
    for row in rows {
        let blob = row.map_err(sqlite_err)?;
        child_ids.push(blob_to_task_id(&blob)?);
    }
    Ok(child_ids)
}

/// A no-op if the edge doesn't exist, matching `StoreTx::remove_parent_edge`'s
/// documented semantics.
pub(crate) fn remove_parent_edge(
    tx: &Transaction,
    parent: TaskId,
    child: TaskId,
) -> Result<(), StoreError> {
    let parent_blob = id_to_blob(parent);
    let child_blob = id_to_blob(child);
    tx.execute(
        "DELETE FROM parent_edges WHERE parent_id = ?1 AND child_id = ?2",
        params![parent_blob.as_slice(), child_blob.as_slice()],
    )
    .map_err(sqlite_err)?;
    Ok(())
}
