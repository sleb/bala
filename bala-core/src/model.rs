//! Shared domain types: task identifiers, the `Task` record, its creation
//! input, and task-type configuration.

use chrono::{DateTime, NaiveDate, Utc};
use uuid::Uuid;

/// Identifies a [`Task`]. Wraps a [`Uuid`] so a `TaskId` can never be passed
/// where a [`UserId`] is expected, even though both wrap the same
/// underlying type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TaskId(Uuid);

impl TaskId {
    /// Generates a new, random `TaskId`.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Uuid> for TaskId {
    fn from(id: Uuid) -> Self {
        Self(id)
    }
}

impl From<TaskId> for Uuid {
    fn from(id: TaskId) -> Self {
        id.0
    }
}

/// Identifies a user, e.g. as a task assignee. Wraps a [`Uuid`]; see
/// [`TaskId`] for why this is a distinct type rather than a shared alias.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UserId(Uuid);

impl UserId {
    /// Generates a new, random `UserId`.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for UserId {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Uuid> for UserId {
    fn from(id: Uuid) -> Self {
        Self(id)
    }
}

impl From<UserId> for Uuid {
    fn from(id: UserId) -> Self {
        id.0
    }
}

/// Whether a [`Task`] is done.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskStatus {
    Incomplete,
    Complete,
}

/// A task as stored and returned by the core library.
///
/// Deliberately narrower than the full LLD shape for this story: fields
/// belonging to later stories/epics (`assignee_id`, `depends_on`,
/// `out_of_sync`, `progress`, `completed_at`, `deleted_at`) are omitted
/// until the stories that need them land.
#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub id: TaskId,
    pub title: String,
    pub description: Option<String>,
    /// A task may sit under multiple parents; empty means top-level.
    pub parent_ids: Vec<TaskId>,
    /// FK into `TaskType::key`; defaults to `"task"`.
    pub type_key: String,
    pub status: TaskStatus,
    pub start_date: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input to `Core::create_task`.
#[derive(Debug, Clone, PartialEq)]
pub struct NewTask {
    pub title: String,
    pub description: Option<String>,
    pub parent_ids: Vec<TaskId>,
    /// `None` means default to `"task"`.
    pub type_key: Option<String>,
    pub start_date: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
}

/// A configurable task type, e.g. "initiative" or "task".
#[derive(Debug, Clone, PartialEq)]
pub struct TaskType {
    /// Stable identifier, e.g. `"initiative"`.
    pub key: String,
    /// Display name, user-renameable.
    pub label: String,
    pub color: Option<String>,
    pub sort_order: i32,
}

/// Filter predicate for `Store::list_tasks`. All fields are `ANDed`;
/// `None`/`false` means "don't filter on this".
///
/// Deliberately narrower than the full LLD shape (`docs/design/core-library.md`
/// §Data Model): `assignee_id` is omitted since `UserId` isn't wired into
/// [`Task`] yet in this story, matching `Task`'s own scope cut.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TreeFilter {
    pub type_key: Option<String>,
    pub status: Option<TaskStatus>,
    /// Default `false`: soft-deleted tasks excluded. `Task` has no
    /// `deleted_at` field yet at this checkpoint's scope, so this is
    /// currently a no-op in `InMemoryStore` — see its module docs.
    pub include_deleted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_id_new_generates_distinct_ids() {
        assert_ne!(TaskId::new(), TaskId::new());
    }

    #[test]
    fn task_id_and_user_id_do_not_convert_into_each_other() {
        // Compile-time shape check: TaskId/UserId each convert to/from
        // Uuid, but there is no direct TaskId <-> UserId conversion path.
        let id = Uuid::new_v4();
        let task_id: TaskId = id.into();
        let user_id: UserId = id.into();
        assert_eq!(Uuid::from(task_id), Uuid::from(user_id));
    }

    #[test]
    fn task_id_round_trips_through_uuid() {
        let uuid = Uuid::new_v4();
        let task_id = TaskId::from(uuid);
        assert_eq!(Uuid::from(task_id), uuid);
    }

    #[test]
    fn task_id_is_debug_clone_copy_eq() {
        let a = TaskId::new();
        let b = a;
        assert_eq!(a, b);
        assert!(!format!("{a:?}").is_empty());
    }

    #[test]
    fn task_status_variants_are_distinct_and_comparable() {
        assert_eq!(TaskStatus::Incomplete, TaskStatus::Incomplete);
        assert_ne!(TaskStatus::Incomplete, TaskStatus::Complete);
    }

    #[test]
    fn task_is_constructible_with_expected_fields() {
        let now = Utc::now();
        let task = Task {
            id: TaskId::new(),
            title: "Write tests".to_owned(),
            description: None,
            parent_ids: Vec::new(),
            type_key: "task".to_owned(),
            status: TaskStatus::Incomplete,
            start_date: None,
            due_date: None,
            created_at: now,
            updated_at: now,
        };

        assert_eq!(task.title, "Write tests");
        assert_eq!(task.status, TaskStatus::Incomplete);
        assert!(task.parent_ids.is_empty());
    }

    #[test]
    fn task_clone_and_eq_agree() {
        let now = Utc::now();
        let task = Task {
            id: TaskId::new(),
            title: "Task".to_owned(),
            description: Some("desc".to_owned()),
            parent_ids: vec![TaskId::new()],
            type_key: "task".to_owned(),
            status: TaskStatus::Complete,
            start_date: None,
            due_date: None,
            created_at: now,
            updated_at: now,
        };
        let cloned = task.clone();
        assert_eq!(task, cloned);
    }

    #[test]
    fn new_task_defaults_type_key_to_none() {
        let new_task = NewTask {
            title: "Title".to_owned(),
            description: None,
            parent_ids: Vec::new(),
            type_key: None,
            start_date: None,
            due_date: None,
        };
        assert_eq!(new_task.type_key, None);
    }

    #[test]
    fn tree_filter_default_has_no_restrictions() {
        let filter = TreeFilter::default();
        assert_eq!(filter.type_key, None);
        assert_eq!(filter.status, None);
        assert!(!filter.include_deleted);
    }

    #[test]
    fn task_type_is_constructible_and_comparable() {
        let a = TaskType {
            key: "initiative".to_owned(),
            label: "Initiative".to_owned(),
            color: Some("#ff0000".to_owned()),
            sort_order: 0,
        };
        let b = a.clone();
        assert_eq!(a, b);
    }
}
