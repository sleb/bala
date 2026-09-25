//! `parent_edges` queries.
//!
//! `dependency_edges` exists in the schema (LLD-2 §Schema) but has no
//! module of its own yet — it sits unused until the dependency-edge
//! `StoreTx` methods land in a later story/epic that actually needs them.

use bala_core::{Parents, Placement, StoreError, TaskId};
use rusqlite::{OptionalExtension, Transaction, params};

use crate::convert::{blob_to_task_id, id_to_blob};
use crate::task::sqlite_err;

pub(crate) fn list_parent_edges(tx: &Transaction, id: TaskId) -> Result<Vec<TaskId>, StoreError> {
    let id_blob = id_to_blob(id);
    let mut stmt = tx
        .prepare("SELECT parent_id FROM parent_edges WHERE child_id = ?1 AND parent_id IS NOT NULL")
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

/// Next free `position` under `parent` (`None` = the root list): max + 1, or
/// 0 when there are no siblings. Tombstoned siblings keep their edges and so
/// keep their positions.
fn next_position(tx: &Transaction, parent: Option<&[u8]>) -> Result<i64, StoreError> {
    tx.query_row(
        "SELECT COALESCE(MAX(position) + 1, 0) FROM parent_edges WHERE parent_id IS ?1",
        params![parent],
        |row| row.get(0),
    )
    .map_err(sqlite_err)
}

/// Position for `child` among `parent`'s children. `After(sibling)` takes
/// the slot right after that sibling, shifting later siblings up by one; it
/// falls back to the end when the sibling is the child itself or is not a
/// child of `parent`.
fn insertion_position(
    tx: &Transaction,
    parent: Option<&[u8]>,
    child: TaskId,
    placement: Placement,
) -> Result<i64, StoreError> {
    if let Placement::After(sibling) = placement
        && sibling != child
    {
        let sibling_blob = id_to_blob(sibling);
        let sibling_pos: Option<i64> = tx
            .query_row(
                "SELECT position FROM parent_edges WHERE parent_id IS ?1 AND child_id = ?2",
                params![parent, sibling_blob.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_err)?;
        if let Some(pos) = sibling_pos {
            tx.execute(
                "UPDATE parent_edges SET position = position + 1 \
                 WHERE parent_id IS ?1 AND position > ?2",
                params![parent, pos],
            )
            .map_err(sqlite_err)?;
            return Ok(pos + 1);
        }
    }
    next_position(tx, parent)
}

/// Every `(parent, child)` edge grouped by parent (`NULL` first) in
/// position order.
pub(crate) fn list_all_child_edges(
    tx: &Transaction,
) -> Result<Vec<(Option<TaskId>, TaskId)>, StoreError> {
    let mut stmt = tx
        .prepare("SELECT parent_id, child_id FROM parent_edges ORDER BY parent_id, position")
        .map_err(sqlite_err)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, Option<Vec<u8>>>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .map_err(sqlite_err)?;
    let mut edges = Vec::new();
    for row in rows {
        let (parent, child) = row.map_err(sqlite_err)?;
        edges.push((
            parent.as_deref().map(blob_to_task_id).transpose()?,
            blob_to_task_id(&child)?,
        ));
    }
    Ok(edges)
}

/// Replaces `child`'s whole parent set: edges outside `parents` are deleted,
/// edges already present are left untouched (keeping their position), and
/// missing ones are inserted per `placement` (default: the end) among their
/// parent's siblings.
/// `Parents::TopLevel` is one NULL-parent edge; a `TopLevel` child that
/// already has it is left alone, and any real edge is removed. The caller's
/// transaction makes this atomic.
pub(crate) fn replace_parent_edges(
    tx: &Transaction,
    child: TaskId,
    parents: &Parents,
    placement: Placement,
) -> Result<(), StoreError> {
    let child_blob = id_to_blob(child);

    // Desired parents: `None` = the NULL edge.
    let wanted: Vec<Option<TaskId>> = match parents {
        Parents::TopLevel => vec![None],
        Parents::Under(ids) => ids.iter().copied().map(Some).collect(),
    };

    let mut existing: Vec<Option<TaskId>> = Vec::new();
    {
        let mut stmt = tx
            .prepare("SELECT parent_id FROM parent_edges WHERE child_id = ?1")
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map(params![child_blob.as_slice()], |row| {
                row.get::<_, Option<Vec<u8>>>(0)
            })
            .map_err(sqlite_err)?;
        for row in rows {
            existing.push(
                row.map_err(sqlite_err)?
                    .as_deref()
                    .map(blob_to_task_id)
                    .transpose()?,
            );
        }
    }

    for old in existing.iter().filter(|e| !wanted.contains(e)) {
        let old_blob = old.map(id_to_blob);
        tx.execute(
            "DELETE FROM parent_edges WHERE child_id = ?1 AND parent_id IS ?2",
            params![
                child_blob.as_slice(),
                old_blob.as_ref().map(<[u8; 16]>::as_slice)
            ],
        )
        .map_err(sqlite_err)?;
    }

    let mut added: Vec<Option<TaskId>> = Vec::new();
    for new in wanted.iter().filter(|w| !existing.contains(w)) {
        if added.contains(new) {
            continue; // duplicate id in `parents`
        }
        added.push(*new);
        let new_blob = new.map(id_to_blob);
        let parent_slice = new_blob.as_ref().map(<[u8; 16]>::as_slice);
        let position = insertion_position(tx, parent_slice, child, placement)?;
        tx.execute(
            "INSERT INTO parent_edges (parent_id, child_id, position) VALUES (?1, ?2, ?3)",
            params![parent_slice, child_blob.as_slice(), position],
        )
        .map_err(sqlite_err)?;
    }
    Ok(())
}

/// Lists `parent`'s children ordered by `position`; `None` lists the roots
/// (children of the NULL parent). Tombstoned tasks are included — callers
/// filter through `get_task`.
pub(crate) fn list_child_edges(
    tx: &Transaction,
    parent: Option<TaskId>,
) -> Result<Vec<TaskId>, StoreError> {
    let parent_blob = parent.map(id_to_blob);
    let mut stmt = tx
        .prepare("SELECT child_id FROM parent_edges WHERE parent_id IS ?1 ORDER BY position")
        .map_err(sqlite_err)?;
    let rows = stmt
        .query_map(
            params![parent_blob.as_ref().map(<[u8; 16]>::as_slice)],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .map_err(sqlite_err)?;

    let mut child_ids = Vec::new();
    for row in rows {
        let blob = row.map_err(sqlite_err)?;
        child_ids.push(blob_to_task_id(&blob)?);
    }
    Ok(child_ids)
}

/// Swaps the `position` of `a` and `b` under `parent`; no-op unless both are
/// children of `parent`. No unique index covers `(parent_id, position)`, so
/// two plain updates suffice.
pub(crate) fn swap_child_positions(
    tx: &Transaction,
    parent: Option<TaskId>,
    a: TaskId,
    b: TaskId,
) -> Result<(), StoreError> {
    let parent_blob = parent.map(id_to_blob);
    let parent_slice = parent_blob.as_ref().map(<[u8; 16]>::as_slice);
    let (a_blob, b_blob) = (id_to_blob(a), id_to_blob(b));
    let position = |child: &[u8]| -> Result<Option<i64>, StoreError> {
        tx.query_row(
            "SELECT position FROM parent_edges WHERE parent_id IS ?1 AND child_id = ?2",
            params![parent_slice, child],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite_err)
    };
    let (Some(pa), Some(pb)) = (position(&a_blob)?, position(&b_blob)?) else {
        return Ok(());
    };
    for (child, pos) in [(&a_blob, pb), (&b_blob, pa)] {
        tx.execute(
            "UPDATE parent_edges SET position = ?3 WHERE parent_id IS ?1 AND child_id = ?2",
            params![parent_slice, child.as_slice(), pos],
        )
        .map_err(sqlite_err)?;
    }
    Ok(())
}
