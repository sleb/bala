//! Error types for `bala-core`. No `anyhow`/string errors cross this
//! boundary — `bala-core` is a library, not a binary.

use chrono::NaiveDate;

use crate::model::TaskId;
use crate::store::StoreError;

/// Errors the `Core` facade can return.
///
/// This is a subset of the full taxonomy in `docs/design/core-library.md`
/// §Error Taxonomy: only the variants needed by Story 1.1 (create a task).
/// Later stories add `CircularHierarchy`, `DependsOnRelative`,
/// `CircularDependency`, and `IncompleteChildren`.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("task {0:?} not found")]
    NotFound(TaskId),

    #[error("title must not be empty")]
    EmptyTitle,

    #[error("due date {due} is before start date {start}")]
    InvalidDateRange { start: NaiveDate, due: NaiveDate },

    #[error("unknown task type {0:?}")]
    UnknownTaskType(String),

    #[error(transparent)]
    Store(#[from] StoreError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_display_includes_task_id_debug() {
        let id = TaskId::new();
        let err = CoreError::NotFound(id);
        assert_eq!(err.to_string(), format!("task {id:?} not found"));
    }

    #[test]
    fn empty_title_display_message() {
        assert_eq!(CoreError::EmptyTitle.to_string(), "title must not be empty");
    }

    #[test]
    fn invalid_date_range_display_message() {
        let start = NaiveDate::from_ymd_opt(2026, 1, 10).unwrap();
        let due = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let err = CoreError::InvalidDateRange { start, due };
        assert_eq!(
            err.to_string(),
            format!("due date {due} is before start date {start}")
        );
    }

    #[test]
    fn unknown_task_type_display_includes_debug_of_key() {
        let err = CoreError::UnknownTaskType("bogus".to_owned());
        assert_eq!(err.to_string(), "unknown task type \"bogus\"");
    }

    #[test]
    fn store_error_display_is_transparent() {
        let store_err = StoreError::Backend("disk full".to_owned());
        let expected = store_err.to_string();
        let err: CoreError = store_err.into();
        assert_eq!(err.to_string(), expected);
    }

    #[test]
    fn store_error_converts_via_from_into_core_error() {
        let err: CoreError = StoreError::Backend("boom".to_owned()).into();
        assert!(matches!(err, CoreError::Store(_)));
    }
}
