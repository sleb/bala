//! Error types for `bala-core`. No `anyhow`/string errors cross this
//! boundary — `bala-core` is a library, not a binary.

use chrono::NaiveDate;

use crate::model::{TaskId, UserId};
use crate::store::StoreError;

/// Errors the `Core` facade can return.
///
/// This is a subset of the full taxonomy in `docs/design/core-library.md`
/// §Error Taxonomy: the dependency variants (`DependsOnRelative`,
/// `CircularDependency`) are added when task dependencies are implemented.
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

    #[error("user name must not be empty")]
    EmptyUserName,

    #[error("unknown user {0:?}")]
    UnknownUser(UserId),

    #[error(transparent)]
    Store(#[from] StoreError),

    #[error(
        "{task:?} has incomplete children: {incomplete:?}; pass cascade=true or complete them first"
    )]
    IncompleteChildren {
        task: TaskId,
        incomplete: Vec<TaskId>,
    },

    #[error("moving {task:?} under {attempted_parent:?} would make it its own ancestor")]
    CircularHierarchy {
        task: TaskId,
        attempted_parent: TaskId,
    },

    #[error("{task:?} is not a child of {}", fmt_parent(*parent))]
    NotUnderParent {
        task: TaskId,
        parent: Option<TaskId>,
    },
}

/// Renders a sibling-list key: the parent id, or "the top level" for `None`.
fn fmt_parent(parent: Option<TaskId>) -> String {
    parent.map_or_else(|| "the top level".to_owned(), |p| format!("{p:?}"))
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
    fn empty_user_name_display_message() {
        assert_eq!(
            CoreError::EmptyUserName.to_string(),
            "user name must not be empty"
        );
    }

    #[test]
    fn unknown_user_display_includes_user_id_debug() {
        let id = UserId::new();
        let err = CoreError::UnknownUser(id);
        assert_eq!(err.to_string(), format!("unknown user {id:?}"));
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

    #[test]
    fn circular_hierarchy_display_message() {
        let task = TaskId::new();
        let attempted_parent = TaskId::new();
        let err = CoreError::CircularHierarchy {
            task,
            attempted_parent,
        };
        assert_eq!(
            err.to_string(),
            format!("moving {task:?} under {attempted_parent:?} would make it its own ancestor")
        );
    }

    #[test]
    fn incomplete_children_display_message() {
        let task = TaskId::new();
        let incomplete = vec![TaskId::new(), TaskId::new()];
        let err = CoreError::IncompleteChildren {
            task,
            incomplete: incomplete.clone(),
        };
        assert_eq!(
            err.to_string(),
            format!(
                "{task:?} has incomplete children: {incomplete:?}; pass cascade=true or complete them first"
            )
        );
    }

    #[test]
    fn not_under_parent_display_names_the_parent() {
        let task = TaskId::new();
        let parent = TaskId::new();
        let err = CoreError::NotUnderParent {
            task,
            parent: Some(parent),
        };
        assert_eq!(
            err.to_string(),
            format!("{task:?} is not a child of {parent:?}")
        );
    }

    #[test]
    fn not_under_parent_display_names_top_level_for_none() {
        let task = TaskId::new();
        let err = CoreError::NotUnderParent { task, parent: None };
        assert_eq!(
            err.to_string(),
            format!("{task:?} is not a child of the top level")
        );
    }
}
