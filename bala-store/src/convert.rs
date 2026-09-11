//! Row <-> domain-type conversions shared by [`crate::task`], [`crate::edges`],
//! and [`crate::types`].
//!
//! IDs are stored as `BLOB(16)` raw UUID bytes (LLD-2 §Decision); dates and
//! timestamps as ISO-8601 `TEXT` (LLD-2 §Schema).

use bala_core::{StoreError, TaskId, TaskStatus};
use chrono::{DateTime, NaiveDate, Utc};
use uuid::Uuid;

/// Maps any lower-level failure (bad blob length, unparseable date/status
/// text) into the one variant `bala_core::StoreError` currently has.
///
/// `bala-core`'s `StoreError` is a Checkpoint-1 placeholder (`Backend(String)`
/// only) — see this crate's module docs for why `bala-store` maps into it
/// via `.to_string()` rather than growing its own separate error type.
pub(crate) fn corrupt(context: &str, detail: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(format!(
        "stored data could not be decoded ({context}): {detail}"
    ))
}

pub(crate) fn id_to_blob(id: TaskId) -> [u8; 16] {
    *Uuid::from(id).as_bytes()
}

pub(crate) fn blob_to_task_id(bytes: &[u8]) -> Result<TaskId, StoreError> {
    let array: [u8; 16] = bytes.try_into().map_err(|_| {
        corrupt(
            "task id",
            format!("blob is {} bytes, expected 16", bytes.len()),
        )
    })?;
    Ok(TaskId::from(Uuid::from_bytes(array)))
}

pub(crate) fn status_to_text(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Incomplete => "incomplete",
        TaskStatus::Complete => "complete",
    }
}

pub(crate) fn status_from_text(text: &str) -> Result<TaskStatus, StoreError> {
    match text {
        "incomplete" => Ok(TaskStatus::Incomplete),
        "complete" => Ok(TaskStatus::Complete),
        other => Err(corrupt("task status", other)),
    }
}

pub(crate) fn date_to_text(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

pub(crate) fn date_from_text(text: &str) -> Result<NaiveDate, StoreError> {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").map_err(|err| corrupt("date", err))
}

pub(crate) fn timestamp_to_text(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339()
}

pub(crate) fn timestamp_from_text(text: &str) -> Result<DateTime<Utc>, StoreError> {
    DateTime::parse_from_rfc3339(text)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|err| corrupt("timestamp", err))
}
