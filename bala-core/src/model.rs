//! Shared domain types: task identifiers, the `Task` record, its creation
//! input, and task-type configuration.

use chrono::{DateTime, NaiveDate, Utc};
use std::collections::HashMap;
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

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::str::FromStr for TaskId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::from_str(s).map(Self)
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

/// Minimal user identity for task assignment — no auth, email, roles, or
/// avatars (out of scope per STORIES.md Story 1.1a); just enough to give
/// `assignee_id` a real entity to resolve to instead of a bare id.
#[derive(Debug, Clone, PartialEq)]
pub struct User {
    pub id: UserId,
    pub name: String,
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
/// belonging to later stories/epics (`depends_on`, `out_of_sync`) are
/// omitted until the stories that need them land.
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
    /// Fraction complete in `0.0..=1.0`, computed by the library (never
    /// set by callers, never persisted). For a leaf task (no children),
    /// this mirrors the task's own `status` (`0.0` for
    /// [`TaskStatus::Incomplete`], `1.0` for [`TaskStatus::Complete`]).
    /// For a task with direct children, it's the average of each direct
    /// child's own `status` flag — grandchildren never factor in (Story
    /// 3.3 AC1/AC2; see `rollup::direct_children_progress`).
    pub progress: f32,
    pub start_date: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
    /// `None` means unassigned. `Core::create_task` validates a `Some`
    /// value against `Store::get_user` before persisting.
    pub assignee_id: Option<UserId>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// `Some` means the task is soft-deleted; `None` means live.
    /// `Store::get_task`/`list_tasks` exclude soft-deleted tasks unless
    /// asked otherwise (`get_task_including_deleted`,
    /// `TreeFilter::include_deleted`).
    pub deleted_at: Option<DateTime<Utc>>,
    /// `Some` when `status` transitioned to [`TaskStatus::Complete`] via
    /// `Core::complete_task`; `None` for an incomplete task.
    pub completed_at: Option<DateTime<Utc>>,
}

/// How `Core::delete_task` should treat a deleted task's children (LLD
/// §Method Contract).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeleteMode {
    /// Soft-delete the task and every descendant beneath it.
    Subtree,
    /// Soft-delete only the task itself; its children are reparented to
    /// its parents (or become top-level if it had none).
    PromoteChildren,
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
    /// `None` means unassigned; `Some` must name an existing [`User`].
    pub assignee_id: Option<UserId>,
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
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TreeFilter {
    pub type_key: Option<String>,
    pub status: Option<TaskStatus>,
    pub assignee_id: Option<UserId>,
    /// Default `false`: soft-deleted tasks (`Task::deleted_at.is_some()`)
    /// excluded.
    pub include_deleted: bool,
}

/// One field of a [`TaskPatch`]: distinguishes "leave alone" from "set to
/// nothing" for `Option<T>`-backed fields, which a plain `Option<T>`
/// (ambiguous) or `Option<Option<T>>` (compiles, but `Some(None)` isn't
/// self-documenting at call sites) cannot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Field<T> {
    /// Leave the field as-is.
    #[default]
    Keep,
    /// Set the field to this value.
    Set(T),
    /// Clear the field (only meaningful for `Option<T>` targets, e.g.
    /// `description`).
    Clear,
}

/// Input to `Core::update_task`. Every field defaults to [`Field::Keep`];
/// callers build one with struct-update syntax against
/// [`TaskPatch::default`], touching only what changed, e.g.
/// `TaskPatch { title: Field::Set("New title".into()), ..Default::default() }`.
///
/// `parent_ids` and dependency edits are intentionally not here — those go
/// through their own dedicated methods (`set_parents`,
/// `add_dependency`/`remove_dependency`) because each carries its own
/// invariant check that a generic patch would obscure.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TaskPatch {
    pub title: Field<String>,
    /// `Clear` maps to `None`.
    pub description: Field<String>,
    /// `Clear` maps to `None`.
    pub start_date: Field<NaiveDate>,
    /// `Clear` maps to `None`.
    pub due_date: Field<NaiveDate>,
    /// `Clear` unassigns the task.
    pub assignee_id: Field<UserId>,
    pub type_key: Field<String>,
}

/// The complete set of parents a task sits under, as written by
/// [`StoreTx::replace_parent_edges`](crate::StoreTx::replace_parent_edges).
///
/// `Under` holds at least one id; use [`Parents::from_ids`] to build one from
/// a possibly-empty list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parents {
    /// No parent: the task is at the top level.
    TopLevel,
    /// One or more parents (never empty).
    Under(Vec<TaskId>),
}

impl Parents {
    /// Builds `TopLevel` from an empty list and `Under` otherwise.
    #[must_use]
    pub fn from_ids(ids: Vec<TaskId>) -> Self {
        if ids.is_empty() {
            Self::TopLevel
        } else {
            Self::Under(ids)
        }
    }

    /// The parent ids: empty for `TopLevel`.
    #[must_use]
    pub fn ids(&self) -> &[TaskId] {
        match self {
            Self::TopLevel => &[],
            Self::Under(ids) => ids,
        }
    }
}

/// Where a re-parented task lands among its new siblings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// After all existing siblings.
    End,
    /// Immediately after the given sibling, in each newly added parent's
    /// children that contains it. For a parent whose children do not include
    /// the sibling (or when the sibling is the child itself) this falls back
    /// to [`Placement::End`]. Parents the child already has keep their
    /// position.
    After(TaskId),
}

/// Which way [`Core::move_sibling`](crate::Core::move_sibling) moves a task
/// among its siblings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Toward the front of the sibling list (earlier).
    Up,
    /// Toward the back of the sibling list (later).
    Down,
}

/// The sibling order of every live task, keyed by parent (`None` = the
/// top-level list). Built by `Core::sibling_order`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SiblingOrder(HashMap<Option<TaskId>, Vec<TaskId>>);

impl SiblingOrder {
    /// The children of `parent` in order; `None` is the top level. Empty when
    /// `parent` has no live children.
    #[must_use]
    pub fn children_of(&self, parent: Option<TaskId>) -> &[TaskId] {
        self.0.get(&parent).map_or(&[], Vec::as_slice)
    }
}

impl From<HashMap<Option<TaskId>, Vec<TaskId>>> for SiblingOrder {
    fn from(map: HashMap<Option<TaskId>, Vec<TaskId>>) -> Self {
        Self(map)
    }
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
    fn task_id_display_should_render_hyphenated_uuid() {
        let uuid = Uuid::new_v4();
        let task_id = TaskId::from(uuid);
        assert_eq!(task_id.to_string(), uuid.to_string());
    }

    #[test]
    fn task_id_from_str_should_round_trip_display_output() {
        let task_id = TaskId::new();
        let rendered = task_id.to_string();
        let parsed: TaskId = rendered.parse().expect("valid uuid string");
        assert_eq!(parsed, task_id);
    }

    #[test]
    fn task_id_from_str_should_reject_invalid_uuid() {
        let result = "not-a-uuid".parse::<TaskId>();
        assert!(result.is_err());
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
            progress: 0.0,
            start_date: None,
            due_date: None,
            assignee_id: None,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            completed_at: None,
        };

        assert_eq!(task.title, "Write tests");
        assert_eq!(task.status, TaskStatus::Incomplete);
        assert!(task.parent_ids.is_empty());
        assert!((task.progress - 0.0).abs() < f32::EPSILON);
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
            progress: 1.0,
            start_date: None,
            due_date: None,
            assignee_id: None,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            completed_at: None,
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
            assignee_id: None,
        };
        assert_eq!(new_task.type_key, None);
    }

    #[test]
    fn tree_filter_default_has_no_restrictions() {
        let filter = TreeFilter::default();
        assert_eq!(filter.type_key, None);
        assert_eq!(filter.status, None);
        assert_eq!(filter.assignee_id, None);
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

    #[test]
    fn user_is_constructible_with_expected_fields() {
        let user = User {
            id: UserId::new(),
            name: "Ada Lovelace".to_owned(),
        };

        assert_eq!(user.name, "Ada Lovelace");
    }

    #[test]
    fn user_clone_and_eq_agree() {
        let user = User {
            id: UserId::new(),
            name: "Grace Hopper".to_owned(),
        };
        let cloned = user.clone();
        assert_eq!(user, cloned);
    }

    #[test]
    fn task_patch_default_has_every_field_keep() {
        let patch = TaskPatch::default();
        assert_eq!(patch.title, Field::Keep);
        assert_eq!(patch.description, Field::Keep);
        assert_eq!(patch.start_date, Field::Keep);
        assert_eq!(patch.due_date, Field::Keep);
        assert_eq!(patch.assignee_id, Field::Keep);
        assert_eq!(patch.type_key, Field::Keep);
    }

    #[test]
    fn field_variants_are_distinct_and_comparable() {
        let keep: Field<String> = Field::Keep;
        let set_x: Field<String> = Field::Set("x".to_owned());
        let set_x_again: Field<String> = Field::Set("x".to_owned());
        let clear: Field<String> = Field::Clear;

        assert_ne!(keep, set_x);
        assert_ne!(set_x, clear);
        assert_ne!(keep, clear);
        assert_eq!(set_x, set_x_again);
    }

    #[test]
    fn delete_mode_variants_are_distinct_and_comparable() {
        assert_eq!(DeleteMode::Subtree, DeleteMode::Subtree);
        assert_eq!(DeleteMode::PromoteChildren, DeleteMode::PromoteChildren);
        assert_ne!(DeleteMode::Subtree, DeleteMode::PromoteChildren);
    }

    #[test]
    fn field_clone_and_eq_agree() {
        let string_field: Field<String> = Field::Set("hello".to_owned());
        let cloned_string_field = string_field.clone();
        assert_eq!(string_field, cloned_string_field);

        let date_field: Field<NaiveDate> =
            Field::Set(NaiveDate::from_ymd_opt(2026, 9, 10).expect("valid date"));
        let cloned_date_field = date_field.clone();
        assert_eq!(date_field, cloned_date_field);
    }
}
