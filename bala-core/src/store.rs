//! The persistence trait boundary (LLD §Storage Boundary).
//!
//! The full LLD contract also has dependency-edge methods; they are not
//! declared yet and get added alongside the dependency features that call
//! them.

use crate::model::{Parents, Placement, Task, TaskId, TaskType, TreeFilter, User, UserId};

/// Errors from the storage boundary.
///
/// A single catch-all variant for now: backends map every failure into
/// `Backend` with a message. Distinguishing failure modes (e.g. corrupt
/// data vs. an unavailable backend, per the LLD's proposed taxonomy) means
/// adding variants here.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("store backend error: {0}")]
    Backend(String),
}

/// A persistence backend, entered one [`StoreTx`] at a time.
///
/// Not a network contract (LLD §Storage Boundary) — a Rust trait so
/// `bala-core` is testable against an in-memory fake ([`InMemoryStore`],
/// see the `in_memory_store` module) today, and a real embedded-engine
/// implementation is just another impl of this trait.
///
/// [`InMemoryStore`]: crate::InMemoryStore
pub trait Store {
    /// Runs `f` against a transactional handle, returning whatever `f`
    /// returns.
    ///
    /// # Errors
    ///
    /// Returns `Err` if `f` returns `Err`, or if the backend itself fails
    /// to complete the transaction.
    fn transaction<T>(
        &self,
        f: impl FnOnce(&mut dyn StoreTx) -> Result<T, StoreError>,
    ) -> Result<T, StoreError>;
}

/// Operations available inside one [`Store::transaction`] call.
pub trait StoreTx {
    /// Looks up a user by id.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend fails. Returns `Ok(None)`, not `Err`,
    /// when no user with `id` exists.
    fn get_user(&mut self, id: UserId) -> Result<Option<User>, StoreError>;

    /// Inserts or replaces the user with `user.id`.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend fails.
    fn put_user(&mut self, user: &User) -> Result<(), StoreError>;

    /// Lists every user.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend fails.
    fn list_users(&mut self) -> Result<Vec<User>, StoreError>;

    /// Looks up a task by id.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend fails. Returns `Ok(None)`, not `Err`,
    /// when no task with `id` exists.
    fn get_task(&mut self, id: TaskId) -> Result<Option<Task>, StoreError>;

    /// Inserts or replaces the task with `task.id`.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend fails.
    fn put_task(&mut self, task: &Task) -> Result<(), StoreError>;

    /// Lists tasks matching `filter`.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend fails.
    fn list_tasks(&mut self, filter: &TreeFilter) -> Result<Vec<Task>, StoreError>;

    /// Lists the ids of `id`'s parents.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend fails. Returns an empty `Vec`, not
    /// `Err`, when `id` has no parents.
    fn list_parent_edges(&mut self, id: TaskId) -> Result<Vec<TaskId>, StoreError>;

    /// Replaces the whole set of `child`'s parent edges with `parents` in
    /// one step. Edges already in the new set are kept, edges not in it are
    /// removed, and missing ones are added. `Parents::TopLevel` gives the
    /// child a single NULL-parent edge (and removes any real ones); an `Under`
    /// set removes the NULL edge. Kept edges keep their position; new ones go
    /// at the end of their parent's children. `placement` says where the child lands among the
    /// siblings of newly added parents.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend fails.
    fn replace_parent_edges(
        &mut self,
        child: TaskId,
        parents: &Parents,
        placement: Placement,
    ) -> Result<(), StoreError>;

    /// Swaps the positions of `a` and `b` among `parent`'s children
    /// (`None` = the top level). Only the `parent` list changes: either
    /// task's edges under other parents keep their positions. Does nothing
    /// if either task is not a child of `parent`.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend fails.
    fn swap_child_positions(
        &mut self,
        parent: Option<TaskId>,
        a: TaskId,
        b: TaskId,
    ) -> Result<(), StoreError>;

    /// Lists the ids of `parent`'s children in position order — the reverse
    /// of [`list_parent_edges`](StoreTx::list_parent_edges). `None` lists the
    /// top-level tasks. Soft-deleted tasks keep their edges and are included.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend fails. Returns an empty `Vec`, not
    /// `Err`, when there are no children.
    fn list_child_edges(&mut self, parent: Option<TaskId>) -> Result<Vec<TaskId>, StoreError>;

    /// Lists every `(parent, child)` edge, grouped by parent and in position
    /// order within each parent (`None` = the top-level list). Soft-deleted
    /// tasks are included. One call replaces a `list_child_edges` per parent,
    /// so whole-hierarchy reads avoid N+1 queries.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend fails.
    fn list_all_child_edges(&mut self) -> Result<Vec<(Option<TaskId>, TaskId)>, StoreError>;

    /// Looks up a task by id, including soft-deleted ones that
    /// [`get_task`](StoreTx::get_task) would filter out.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend fails. Returns `Ok(None)`, not `Err`,
    /// when no task with `id` exists.
    fn get_task_including_deleted(&mut self, id: TaskId) -> Result<Option<Task>, StoreError>;

    /// Lists every configured task type.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend fails.
    fn get_task_types(&mut self) -> Result<Vec<TaskType>, StoreError>;

    /// Inserts or replaces the task type with `t.key`.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend fails.
    fn put_task_type(&mut self, t: &TaskType) -> Result<(), StoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_display_includes_message() {
        let err = StoreError::Backend("disk full".to_owned());
        assert_eq!(err.to_string(), "store backend error: disk full");
    }
}
