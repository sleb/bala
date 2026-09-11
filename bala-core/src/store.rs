//! The persistence trait boundary (LLD §Storage Boundary).
//!
//! [`Store`]/[`StoreTx`] cover what Story 1.1 needs — task put/get/list,
//! parent edges, and task types — plus the edge/soft-delete lookups Story
//! 1.3 ("Delete a Task") needs: `list_child_edges`, `remove_parent_edge`,
//! `get_task_including_deleted`. The full LLD contract also has the
//! dependency-edge methods; those land in later stories that actually use
//! them, per the "don't speculatively build methods this story doesn't
//! need" guidance.

use crate::model::{Task, TaskId, TaskType, TreeFilter, User, UserId};

/// Errors from the storage boundary.
///
/// Placeholder for Checkpoint 1: just enough for `CoreError::Store` to
/// wrap something. Checkpoint 2 builds out the real `Store` trait and will
/// likely extend this with variants for the concrete failure modes it can
/// hit.
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

    /// Records that `child` sits under `parent`.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend fails.
    fn add_parent_edge(&mut self, parent: TaskId, child: TaskId) -> Result<(), StoreError>;

    /// Lists the ids of `id`'s children — the reverse of
    /// [`list_parent_edges`](StoreTx::list_parent_edges).
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend fails. Returns an empty `Vec`, not
    /// `Err`, when `id` has no children.
    fn list_child_edges(&mut self, id: TaskId) -> Result<Vec<TaskId>, StoreError>;

    /// Removes the edge recording that `child` sits under `parent`, leaving
    /// any other parent edges `child` has untouched.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend fails. Removing an edge that doesn't
    /// exist is not an error.
    fn remove_parent_edge(&mut self, parent: TaskId, child: TaskId) -> Result<(), StoreError>;

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
