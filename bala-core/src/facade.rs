//! The `Core` facade (LLD §Decision, §Method Contract): the single entry
//! point every caller (CLI today, Web API later) drives.
//!
//! Task dependencies can be added and removed; date-cascade rescheduling
//! along them is not implemented yet.

use std::collections::{HashMap, HashSet};

use chrono::Utc;

use crate::error::CoreError;
use crate::hierarchy;
use crate::model::{
    DeleteMode, DeleteOutcome, Dependency, DependencyType, Direction, Field, NewTask, Placement,
    SiblingOrder, Task, TaskId, TaskPatch, TaskStatus, TaskType, TreeFilter, User, UserId,
};
use crate::rollup;
use crate::scheduling::check_new_dependency;
use crate::store::{Store, StoreError, StoreTx};

/// The stable key of the default task type every `Core` seeds on
/// construction (LLD §Data Model: `type_key` "defaults to `\"task\"`").
const DEFAULT_TYPE_KEY: &str = "task";

/// Facade over a [`Store`] backend, generic rather than boxed (LLD
/// §Decision: exactly one store implementation is live at a time, so
/// static dispatch costs nothing).
#[derive(Debug)]
pub struct Core<S: Store> {
    store: S,
}

/// Every unit test that builds a `Core` gets the parent-edge invariant
/// checked when it finishes (skipped while unwinding from a failed test).
#[cfg(test)]
impl<S: Store> Drop for Core<S> {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            crate::test_support::assert_edges_valid(&self.store);
        }
    }
}

impl<S: Store> Core<S> {
    /// Constructs a `Core` backed by `store`, seeding the default
    /// `"task"` [`TaskType`] if one isn't already present, so a freshly
    /// constructed `Core` always has a type `create_task` can default to.
    /// Idempotent: constructing another `Core` over a store that already
    /// has the default type leaves it untouched.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend fails while checking or seeding the
    /// default task type.
    pub fn new(store: S) -> Result<Self, CoreError> {
        store.transaction(|tx| {
            let types = tx.get_task_types()?;
            if types.iter().any(|t| t.key == DEFAULT_TYPE_KEY) {
                return Ok(());
            }
            tx.put_task_type(&TaskType {
                key: DEFAULT_TYPE_KEY.to_owned(),
                label: "Task".to_owned(),
                color: None,
                sort_order: 0,
            })
        })?;
        Ok(Self { store })
    }

    /// Creates a user, so `Task::assignee_id` has a real entity to resolve to.
    ///
    /// # Errors
    ///
    /// - [`CoreError::EmptyUserName`] if `name` is empty or
    ///   whitespace-only.
    /// - [`CoreError::Store`] if the backend fails.
    pub fn create_user(&mut self, name: String) -> Result<User, CoreError> {
        if name.trim().is_empty() {
            return Err(CoreError::EmptyUserName);
        }

        let user = User {
            id: UserId::new(),
            name,
        };

        self.store.transaction(|tx| tx.put_user(&user))?;

        Ok(user)
    }

    /// Lists every user.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend fails.
    pub fn list_users(&self) -> Result<Vec<User>, CoreError> {
        Ok(self.store.transaction(|tx| tx.list_users())?)
    }

    /// Creates a task per LLD §Algorithm: validates the
    /// title, type key, date range, the given parent, and the assignee,
    /// then persists the new task and its parent edge. Everything after
    /// the title check, validation and writes alike, runs in a single
    /// store transaction, and the validation errors below are checked in
    /// the order listed ([`CoreError::Store`] can surface from any step).
    ///
    /// # Errors
    ///
    /// - [`CoreError::EmptyTitle`] if `new.title` is empty or
    ///   whitespace-only.
    /// - [`CoreError::UnknownTaskType`] if `new.type_key` (or the
    ///   default) names no configured [`TaskType`].
    /// - [`CoreError::InvalidDateRange`] if both dates are given and
    ///   `due_date` is before `start_date`.
    /// - [`CoreError::NotFound`] if `new.parent_id` is `Some` and names no
    ///   existing, live task.
    /// - [`CoreError::UnknownUser`] if `new.assignee_id` is `Some` and
    ///   names no existing [`User`].
    /// - [`CoreError::Store`] if the backend fails.
    pub fn create_task(&mut self, new: NewTask) -> Result<Task, CoreError> {
        if new.title.trim().is_empty() {
            return Err(CoreError::EmptyTitle);
        }

        let now = Utc::now();
        let mut task = Task {
            id: TaskId::new(),
            title: new.title,
            description: new.description,
            parent_id: new.parent_id,
            type_key: new.type_key.unwrap_or_else(|| DEFAULT_TYPE_KEY.to_owned()),
            status: TaskStatus::Incomplete,
            progress: 0.0,
            start_date: new.start_date,
            due_date: new.due_date,
            assignee_id: new.assignee_id,
            depends_on: Vec::new(),
            created_at: now,
            updated_at: now,
            deleted_at: None,
            completed_at: None,
        };

        // Every check past the title runs inside the same
        // `Store::transaction` closure as the writes, so the whole create
        // is one transaction and, e.g., a parent soft-deleted by a
        // concurrent writer can't have the new task attached under it
        // after passing a check in an earlier transaction (a backend that
        // detects the conflict fails the create with a store error
        // instead, e.g. SQLite's `SQLITE_BUSY`). The date check
        // reads no store state but sits between the type and parent checks
        // to keep the documented error order. Every check precedes
        // `put_task`, since a `Store` need not roll back a closure that
        // returns early. Rejections are threaded out as `Ok(Err(_))`,
        // distinct from backend failures, and unwrapped by the two `?`s
        // below.
        let progress = self.store.transaction(|tx| {
            if !tx.get_task_types()?.iter().any(|t| t.key == task.type_key) {
                return Ok(Err(CoreError::UnknownTaskType(task.type_key.clone())));
            }
            if let (Some(start), Some(due)) = (task.start_date, task.due_date)
                && due < start
            {
                return Ok(Err(CoreError::InvalidDateRange { start, due }));
            }
            if let Some(parent_id) = task.parent_id
                && tx.get_task(parent_id)?.is_none()
            {
                return Ok(Err(CoreError::NotFound(parent_id)));
            }
            if let Some(assignee_id) = task.assignee_id
                && tx.get_user(assignee_id)?.is_none()
            {
                return Ok(Err(CoreError::UnknownUser(assignee_id)));
            }

            // No cycle check: a new id has no descendants, so it cannot be
            // an ancestor of its parent, and the parent's existence was
            // checked just above.
            tx.put_task(&task)?;
            tx.set_parent_edge(task.id, task.parent_id, Placement::End)?;
            // A just-created task has no children of its own yet (nothing
            // can point at `task.id` before this call), so this always
            // degenerates to the same `0.0` a leaf-only placeholder would
            // give — but it's computed via the shared helper, after
            // `put_task`, for consistency with every other `Task`-returning
            // method rather than assumed.
            Ok(Ok(compute_progress(tx, task.id, task.status)?))
        })??;
        task.progress = progress;

        Ok(task)
    }

    /// Looks up a task by id.
    ///
    /// A thin pass-through to [`StoreTx::get_task`]: like every other read
    /// in this module (`update_task`, `delete_task`, `complete_task`), a
    /// soft-deleted task reads as `Ok(None)`, not `Ok(Some(_))` — use
    /// `Store::get_task_including_deleted` directly (as `restore_task`
    /// does) if a tombstoned task must still be found.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend fails.
    pub fn get_task(&self, id: TaskId) -> Result<Option<Task>, CoreError> {
        Ok(self.store.transaction(|tx| {
            let Some(mut task) = tx.get_task(id)? else {
                return Ok(None);
            };
            task.progress = compute_progress(tx, id, task.status)?;
            Ok(Some(task))
        })?)
    }

    /// Lists `id`'s direct children only — not grandchildren or any deeper
    /// descendant.
    ///
    /// A thin pass-through composing [`StoreTx::list_child_edges`] with
    /// [`StoreTx::get_task`]: an id with no recorded child edges returns an
    /// empty `Vec`, not an error, matching `list_child_edges`'s own
    /// convention. A child edge whose task no longer resolves via
    /// `get_task` (soft-deleted or otherwise missing) is silently skipped,
    /// mirroring how `incomplete_descendants` and `mark_complete_subtree`
    /// already treat a missing lookup elsewhere in this module.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend fails.
    pub fn list_children(&self, id: TaskId) -> Result<Vec<Task>, CoreError> {
        Ok(self.store.transaction(|tx| {
            let mut children = tx
                .list_child_edges(Some(id))?
                .into_iter()
                .filter_map(|child_id| tx.get_task(child_id).transpose())
                .collect::<Result<Vec<Task>, StoreError>>()?;
            for child in &mut children {
                child.progress = compute_progress(tx, child.id, child.status)?;
            }
            Ok(children)
        })?)
    }

    /// Lists tasks matching `filter`, with each result's `progress`
    /// overwritten by a direct-children rollup (LLD §Algorithm 4): for each task `Store::list_tasks` returns, its
    /// direct children are fetched (`StoreTx::list_child_edges` +
    /// `StoreTx::get_task`, the same pattern `list_children` uses) and
    /// `rollup::direct_children_progress` averages their `status` flags —
    /// a leaf task's `progress` falls back to its own `status`.
    /// Hierarchy assembly beyond what `Task::parent_id` already carries is
    /// still out of scope.
    ///
    /// The base list and every per-task child lookup run inside the same
    /// `Store::transaction` closure, so the whole read is one atomic
    /// snapshot rather than separate transactions that could observe a
    /// concurrent write differently between the list and a child fetch.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend fails.
    // `TreeFilter` is taken by value to match the LLD §Method Contract
    // signature (`get_tree(&self, filter: TreeFilter)`), which callers
    // build fresh per call rather than reuse.
    #[allow(clippy::needless_pass_by_value)]
    pub fn get_tree(&self, filter: TreeFilter) -> Result<Vec<Task>, CoreError> {
        Ok(self.store.transaction(|tx| {
            let mut tasks = tx.list_tasks(&filter)?;
            sort_by_hierarchy_order(tx, &mut tasks)?;
            for task in &mut tasks {
                let children = tx
                    .list_child_edges(Some(task.id))?
                    .into_iter()
                    .filter_map(|child_id| tx.get_task(child_id).transpose())
                    .collect::<Result<Vec<Task>, StoreError>>()?;
                task.progress = rollup::direct_children_progress(&children, task.status);
            }
            Ok(tasks)
        })?)
    }

    /// The sibling order of every live task, keyed by parent (`None` = top
    /// level), read in one transaction via
    /// [`StoreTx::list_all_child_edges`]. Soft-deleted tasks are skipped.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend fails.
    pub fn sibling_order(&self) -> Result<SiblingOrder, CoreError> {
        Ok(self.store.transaction(|tx| live_sibling_order(tx))?)
    }

    /// Lists every configured task type.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend fails.
    pub fn list_task_types(&self) -> Result<Vec<TaskType>, CoreError> {
        Ok(self.store.transaction(|tx| tx.get_task_types())?)
    }

    /// Updates an existing task per LLD §Algorithm: applies
    /// `patch` field-by-field, validates the resulting state, bumps
    /// `updated_at`, and persists.
    ///
    /// `patch.title` and `patch.type_key` are `String`-backed, not
    /// `Option`-backed, on [`Task`], so [`Field::Clear`] on either is a
    /// defensive no-op equivalent to [`Field::Keep`] rather than a real
    /// path — no caller constructs one.
    ///
    /// Editing never reschedules dependents or subtasks — a task's own
    /// dates and assignment change in isolation, and this always returns a
    /// single-element `Vec` until that cascade lands.
    ///
    /// # Errors
    ///
    /// - [`CoreError::NotFound`] if `id` names no existing task.
    /// - [`CoreError::EmptyTitle`] if the resulting title is empty or
    ///   whitespace-only.
    /// - [`CoreError::UnknownTaskType`] if the resulting `type_key` names
    ///   no configured [`TaskType`].
    /// - [`CoreError::InvalidDateRange`] if the resulting `start_date`
    ///   and `due_date` are both `Some` and `due_date` is before
    ///   `start_date`.
    /// - [`CoreError::UnknownUser`] if `patch.assignee_id` is
    ///   [`Field::Set`] to a value naming no existing [`User`].
    /// - [`CoreError::Store`] if the backend fails.
    pub fn update_task(&mut self, id: TaskId, patch: TaskPatch) -> Result<Vec<Task>, CoreError> {
        // Every lookup, the patch application, validation, and the final
        // `put_task` all run inside one `Store::transaction` closure — not
        // one per step — so a concurrent writer can't slip a change in
        // between our read and our write and have it silently clobbered by
        // a `put_task` built from a now-stale snapshot. Domain-level
        // rejections (`NotFound`, `EmptyTitle`, ...) are threaded out as
        // `Ok(Err(_))`, distinct from backend failures (`StoreError`,
        // propagated via `?` as usual), and unwrapped by the two `?`s
        // below.
        let task = self.store.transaction(|tx| {
            let Some(mut task) = tx.get_task(id)? else {
                return Ok(Err(CoreError::NotFound(id)));
            };

            if let Field::Set(title) = patch.title {
                task.title = title;
            }
            if task.title.trim().is_empty() {
                return Ok(Err(CoreError::EmptyTitle));
            }

            if let Field::Set(type_key) = patch.type_key {
                task.type_key = type_key;
            }
            let types = tx.get_task_types()?;
            if !types.iter().any(|t| t.key == task.type_key) {
                return Ok(Err(CoreError::UnknownTaskType(task.type_key)));
            }

            match patch.description {
                Field::Keep => {}
                Field::Set(description) => task.description = Some(description),
                Field::Clear => task.description = None,
            }

            match patch.start_date {
                Field::Keep => {}
                Field::Set(start_date) => task.start_date = Some(start_date),
                Field::Clear => task.start_date = None,
            }
            match patch.due_date {
                Field::Keep => {}
                Field::Set(due_date) => task.due_date = Some(due_date),
                Field::Clear => task.due_date = None,
            }
            if let (Some(start), Some(due)) = (task.start_date, task.due_date)
                && due < start
            {
                return Ok(Err(CoreError::InvalidDateRange { start, due }));
            }

            match patch.assignee_id {
                Field::Keep => {}
                Field::Set(assignee_id) => {
                    if tx.get_user(assignee_id)?.is_none() {
                        return Ok(Err(CoreError::UnknownUser(assignee_id)));
                    }
                    task.assignee_id = Some(assignee_id);
                }
                Field::Clear => task.assignee_id = None,
            }

            // `patch` never touches `status`, but `progress` is
            // library-computed and not reliably round-tripped by every
            // `Store` impl (`bala-store`'s row conversion always returns
            // `0.0`), so it's re-derived via the shared `compute_progress`
            // rollup on every write rather than trusted from the fetched
            // `task`.
            task.progress = compute_progress(tx, id, task.status)?;
            task.updated_at = Utc::now();
            tx.put_task(&task)?;

            Ok(Ok(task))
        })??;

        Ok(vec![task])
    }

    /// Inserts or replaces the task type with `t.key`.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend fails.
    pub fn upsert_task_type(&mut self, t: TaskType) -> Result<TaskType, CoreError> {
        self.store.transaction(|tx| tx.put_task_type(&t))?;
        Ok(t)
    }

    /// Deletes a task per LLD §Algorithm: tombstones `id`
    /// (`deleted_at` set, never a hard delete) and, per `mode`, either
    /// recursively tombstones every descendant ([`DeleteMode::Subtree`]) or
    /// reparents each child onto `id`'s own parent, leaving it top-level if
    /// `id` had none ([`DeleteMode::PromoteChildren`]).
    ///
    /// Everything runs inside one [`Store::transaction`]. Returns a
    /// [`DeleteOutcome`] split by kind: `deleted` holds every task
    /// tombstoned, `updated` every surviving task whose own fields changed
    /// — the children reparented under `PromoteChildren`; `Subtree` leaves
    /// it empty. Both lists hold only tasks whose own stored row changed,
    /// each at most once, at its final value, and no id is in both. A
    /// parent whose rolled-up `progress` changed only because its children
    /// changed is not included; a caller that shows it re-fetches it.
    ///
    /// # Errors
    ///
    /// - [`CoreError::NotFound`] if `id` names no existing, live task —
    ///   including a task that is already soft-deleted, which is treated
    ///   as not-found rather than tombstoned a second time.
    /// - [`CoreError::Store`] if the backend fails.
    pub fn delete_task(
        &mut self,
        id: TaskId,
        mode: DeleteMode,
    ) -> Result<DeleteOutcome, CoreError> {
        let outcome = self.store.transaction(|tx| {
            // `get_task` (not `get_task_including_deleted`) so a task
            // that's already soft-deleted is treated as not-found here,
            // matching `update_task`'s "not found" idiom rather than
            // re-tombstoning/double-counting it.
            let Some(task) = tx.get_task(id)? else {
                return Ok(Err(CoreError::NotFound(id)));
            };

            let now = Utc::now();
            let mut outcome = DeleteOutcome::default();

            match mode {
                DeleteMode::Subtree => {
                    tombstone_subtree(tx, id, now, &mut outcome.deleted)?;
                }
                DeleteMode::PromoteChildren => {
                    // `id`'s own parent, as read with the task. The loop
                    // below never changes it: it rewrites the edges of
                    // `id`'s children, not `id`'s own.
                    let grandparent = task.parent_id;

                    let mut deleted = task;
                    deleted.deleted_at = Some(now);
                    deleted.updated_at = now;

                    for child in tx.list_child_edges(Some(id))? {
                        if let Err(e) =
                            hierarchy::set_parent(tx, child, grandparent, Placement::End)
                        {
                            return Ok(Err(e));
                        }

                        if let Some(mut child_task) = tx.get_task(child)? {
                            child_task.parent_id = grandparent;
                            // `child`'s own children are untouched by this
                            // reparent (only its *parent* edge changes), so
                            // this is safe to compute at any point in the
                            // loop.
                            child_task.progress = compute_progress(tx, child, child_task.status)?;
                            child_task.updated_at = now;
                            tx.put_task(&child_task)?;
                            outcome.updated.push(child_task);
                        }
                    }

                    // `status` is untouched by a delete, but `progress` is
                    // re-derived rather than trusted from the fetched row —
                    // see `update_task`'s matching comment. Computed here,
                    // after the loop above has already reparented every one
                    // of `deleted`'s children away, so it reflects the
                    // committed (now-empty) child-edge set — matching what
                    // a later independent fetch (e.g. `get_tree` with
                    // `include_deleted: true`) would recompute, rather than
                    // a stale pre-delete snapshot the returned `Task` can no
                    // longer back up.
                    deleted.progress = compute_progress(tx, id, deleted.status)?;
                    tx.put_task(&deleted)?;
                    outcome.deleted.push(deleted);
                }
            }

            Ok(Ok(outcome))
        })??;

        Ok(outcome)
    }

    /// Reopens a completed task: the reverse of `complete_task`'s
    /// Incomplete→Complete transition.
    /// Looks `id` up via [`StoreTx::get_task`] — a soft-deleted task is not
    /// reopenable, matching `delete_task`'s own use of `get_task` rather
    /// than `get_task_including_deleted` — sets `status =`
    /// [`TaskStatus::Incomplete`], clears `completed_at`, bumps
    /// `updated_at`, and persists.
    ///
    /// Idempotent: calling this on a task that's already
    /// [`TaskStatus::Incomplete`] is not an error — `status` and
    /// `completed_at` were already at their target values — but
    /// `updated_at` still bumps, mirroring `restore_task`'s own convention
    /// of always bumping `updated_at` on any successful call regardless of
    /// what the call actually changed.
    ///
    /// # Errors
    ///
    /// - [`CoreError::NotFound`] if `id` names no existing, live task.
    /// - [`CoreError::Store`] if the backend fails.
    pub fn reopen_task(&mut self, id: TaskId) -> Result<Task, CoreError> {
        let task = self.store.transaction(|tx| {
            let Some(mut task) = tx.get_task(id)? else {
                return Ok(Err(CoreError::NotFound(id)));
            };

            task.status = TaskStatus::Incomplete;
            task.progress = compute_progress(tx, id, task.status)?;
            task.completed_at = None;
            task.updated_at = Utc::now();
            tx.put_task(&task)?;

            Ok(Ok(task))
        })??;

        Ok(task)
    }

    /// Restores a soft-deleted task per LLD §Algorithm. This restores only
    /// the `id` row itself: it is not a full undo for `delete_task`, since
    /// any descendants that `delete_task` tombstoned stay tombstoned, and a
    /// descendant restored on its own comes back at the top level, where
    /// the subtree delete left its parent edge (the task the delete was
    /// called on keeps its edge, so it comes back under its parent). Looks
    /// `id` up via [`StoreTx::get_task_including_deleted`] — unlike
    /// `delete_task`'s `get_task`, this must see a tombstoned row, not
    /// just a live one — clears `deleted_at`, bumps `updated_at`, and
    /// persists.
    ///
    /// Idempotent: calling this on a task that's already live (never
    /// deleted, or already restored) is not an error — `deleted_at` was
    /// already `None` and stays `None` — but `updated_at` still bumps,
    /// mirroring `update_task`'s own convention of always bumping
    /// `updated_at` on any successful call regardless of what the call
    /// actually changed.
    ///
    /// # Errors
    ///
    /// - [`CoreError::NotFound`] if `id` names no existing task at all —
    ///   not even a soft-deleted one.
    /// - [`CoreError::Store`] if the backend fails.
    pub fn restore_task(&mut self, id: TaskId) -> Result<Task, CoreError> {
        let task = self.store.transaction(|tx| {
            let Some(mut task) = tx.get_task_including_deleted(id)? else {
                return Ok(Err(CoreError::NotFound(id)));
            };

            task.deleted_at = None;
            // `status` is untouched by a restore, but `progress` is
            // re-derived rather than trusted from the fetched row — see
            // `update_task`'s matching comment.
            task.progress = compute_progress(tx, id, task.status)?;
            task.updated_at = Utc::now();
            tx.put_task(&task)?;

            Ok(Ok(task))
        })??;

        Ok(task)
    }

    /// Completes a task per LLD §Algorithm 5: marks `id`
    /// `status = Complete`, `completed_at = Some(now)`, `updated_at = now`.
    ///
    /// If `id` has any descendant, at any depth, whose `status` is still
    /// [`TaskStatus::Incomplete`], the call is rejected with
    /// [`CoreError::IncompleteChildren`] and nothing is written — unless
    /// `cascade` is `true`, in which case `id`'s entire subtree is marked
    /// complete instead (every not-yet-complete descendant, walked
    /// iteratively so an arbitrarily deep hierarchy can't overflow the
    /// stack). The rejection's `incomplete` list names every one of those
    /// descendants, not just direct children, so it always matches exactly
    /// what passing `cascade: true` would actually touch — a caller (e.g.
    /// the TUI's cascade-confirm prompt) can render an accurate count
    /// straight from this error without walking the hierarchy itself.
    ///
    /// Everything runs inside one [`Store::transaction`]. Returns every
    /// [`Task`] actually completed by this call — a single-element `Vec`
    /// when only `id` itself was completed, or the whole touched subtree
    /// under cascade. A descendant that was already complete before this
    /// call is not re-touched and not included in the result.
    ///
    /// # Errors
    ///
    /// - [`CoreError::NotFound`] if `id` names no existing, live task.
    /// - [`CoreError::IncompleteChildren`] if `id` has a descendant, at any
    ///   depth, that is still incomplete and `cascade` is `false`.
    /// - [`CoreError::Store`] if the backend fails.
    pub fn complete_task(&mut self, id: TaskId, cascade: bool) -> Result<Vec<Task>, CoreError> {
        let touched = self.store.transaction(|tx| {
            // Fetched once and reused below (non-cascade branch) rather than
            // re-fetched, since it's the same row `Store::get_task` already
            // returned for the existence check.
            let Some(task) = tx.get_task(id)? else {
                return Ok(Err(CoreError::NotFound(id)));
            };

            let incomplete = incomplete_descendants(tx, id)?;
            if !incomplete.is_empty() && !cascade {
                return Ok(Err(CoreError::IncompleteChildren {
                    task: id,
                    incomplete,
                }));
            }

            let now = Utc::now();
            let mut touched = Vec::new();
            if cascade {
                mark_complete_subtree(tx, id, now, &mut touched)?;
            } else if task.status != TaskStatus::Complete {
                let mut task = task;
                task.status = TaskStatus::Complete;
                // `incomplete_descendants` above already confirmed every
                // descendant of `id` is Complete (that's exactly why we're
                // in this non-cascade branch at all), so `id`'s direct
                // children are already at their final status — no ordering
                // hazard like `mark_complete_subtree`'s below.
                task.progress = compute_progress(tx, id, task.status)?;
                task.completed_at = Some(now);
                task.updated_at = now;
                tx.put_task(&task)?;
                touched.push(task);
            }

            Ok(Ok(touched))
        })??;

        Ok(touched)
    }

    /// Moves `id` under `parent` per LLD §Method Contract; `None` promotes
    /// it to the top level. `id` lands after the new parent's existing
    /// children, and its subtree moves with it.
    ///
    /// The whole read-check-write runs inside one [`Store::transaction`].
    /// `parent` is checked — for existence and for the hierarchy invariant
    /// via `hierarchy::check_new_parent` — before anything is written, so a
    /// rejected call leaves `id`'s parent edge and row untouched. If `id` is
    /// already under `parent`, nothing is written and the task is returned
    /// as stored: `updated_at` is unchanged and it keeps its position among
    /// its siblings.
    ///
    /// # Errors
    ///
    /// - [`CoreError::NotFound`] if `id` or `parent` names no existing, live
    ///   task; it carries whichever id is missing, `id` first.
    /// - [`CoreError::CircularHierarchy`] if `parent` is `id` itself or one
    ///   of its descendants: the move would make `id` its own ancestor.
    /// - [`CoreError::Store`] if the backend fails.
    pub fn set_parent(&mut self, id: TaskId, parent: Option<TaskId>) -> Result<Task, CoreError> {
        let task = self.store.transaction(|tx| {
            let Some(mut task) = tx.get_task(id)? else {
                return Ok(Err(CoreError::NotFound(id)));
            };

            if let Some(candidate) = parent
                && tx.get_task(candidate)?.is_none()
            {
                return Ok(Err(CoreError::NotFound(candidate)));
            }

            if task.parent_id == parent {
                task.progress = compute_progress(tx, id, task.status)?;
                return Ok(Ok(task));
            }

            if let Err(e) = hierarchy::set_parent(tx, id, parent, Placement::End) {
                return Ok(Err(e));
            }

            task.parent_id = parent;
            // `status` is untouched by a reparent, and neither is `id`'s
            // own children set (only `id`'s *parent* edge changes here), so
            // recomputing via `compute_progress` is safe and leaves every
            // sibling under the old and new parent untouched — this call
            // never writes any row but `id`'s own.
            task.progress = compute_progress(tx, id, task.status)?;
            task.updated_at = Utc::now();
            tx.put_task(&task)?;

            Ok(Ok(task))
        })??;

        Ok(task)
    }

    /// Moves `id` one position among the live children of its parent (the
    /// top level for a task with no parent), toward the front for
    /// [`Direction::Up`] or the back for [`Direction::Down`], by swapping
    /// with the nearest live sibling in that direction (soft-deleted
    /// siblings are skipped). Only the order of the parent's children
    /// changes: `id`'s parent, depth and `updated_at` are untouched. The
    /// whole operation runs in one transaction.
    ///
    /// Returns `true` if the task moved, `false` if it was already at that
    /// end (a no-op: nothing is written and no error is raised).
    ///
    /// # Errors
    ///
    /// - [`CoreError::NotFound`] if `id` names no live task.
    /// - [`CoreError::Store`] if the backend fails.
    pub fn move_sibling(&mut self, id: TaskId, direction: Direction) -> Result<bool, CoreError> {
        let moved = self.store.transaction(|tx| {
            let task = match live_task(tx, id) {
                Ok(task) => task,
                Err(e) => return Ok(Err(e)),
            };
            let live = live_siblings(tx, task.parent_id, id)?;
            let neighbor = live
                .iter()
                .position(|&s| s == id)
                .and_then(|at| match direction {
                    Direction::Up => at.checked_sub(1),
                    Direction::Down => Some(at + 1),
                })
                .and_then(|i| live.get(i).copied());
            let Some(neighbor) = neighbor else {
                return Ok(Ok(false));
            };
            tx.swap_child_positions(task.parent_id, id, neighbor)?;
            Ok(Ok(true))
        })??;
        Ok(moved)
    }

    /// Indents `id` under its nearest live previous sibling among the
    /// children of its parent (the top level for a task with no parent):
    /// `id` leaves that parent and becomes the last child of the sibling.
    /// Only `id`'s own parent edge changes, so its subtree moves with it.
    /// Runs in one transaction.
    ///
    /// Returns `true` if the task moved, `false` if it has no live previous
    /// sibling (a no-op: nothing is written).
    ///
    /// # Errors
    ///
    /// - [`CoreError::NotFound`] if `id` names no live task.
    /// - [`CoreError::CircularHierarchy`] if the previous sibling is a
    ///   descendant of `id`, via the same guard as [`Core::set_parent`]. A
    ///   sibling is never a descendant in a tree, so this cannot arise from
    ///   a valid store.
    /// - [`CoreError::Store`] if the backend fails.
    pub fn indent_task(&mut self, id: TaskId) -> Result<bool, CoreError> {
        let changed = self.store.transaction(|tx| {
            let mut task = match live_task(tx, id) {
                Ok(task) => task,
                Err(e) => return Ok(Err(e)),
            };
            let live = live_siblings(tx, task.parent_id, id)?;
            let at = live.iter().position(|&s| s == id);
            let Some(target) = at.and_then(|i| i.checked_sub(1)).map(|i| live[i]) else {
                return Ok(Ok(false));
            };
            Ok(write_parent(tx, &mut task, Some(target), Placement::End).map(|()| true))
        })??;
        Ok(changed)
    }

    /// Outdents `id` one level: it leaves its parent and joins that
    /// parent's own parent (the top level when the parent has none),
    /// landing immediately after its old parent among the new siblings.
    /// Only `id`'s own parent edge changes, so its subtree moves with it.
    /// Runs in one transaction.
    ///
    /// Returns `true` if the task moved, `false` if it has no parent
    /// (already top level: nothing is written).
    ///
    /// # Errors
    ///
    /// - [`CoreError::NotFound`] if `id` names no live task.
    /// - [`CoreError::CircularHierarchy`] if the move would create a cycle,
    ///   via the same guard as [`Core::set_parent`]. A task's grandparent
    ///   is never its descendant in a tree, so this cannot arise from a
    ///   valid store.
    /// - [`CoreError::Store`] if the backend fails.
    pub fn outdent_task(&mut self, id: TaskId) -> Result<bool, CoreError> {
        let changed = self.store.transaction(|tx| {
            let mut task = match live_task(tx, id) {
                Ok(task) => task,
                Err(e) => return Ok(Err(e)),
            };
            let Some(old_parent) = task.parent_id else {
                return Ok(Ok(false));
            };
            let grandparent = tx.get_parent_edge(old_parent)?;
            Ok(
                write_parent(tx, &mut task, grandparent, Placement::After(old_parent))
                    .map(|()| true),
            )
        })??;
        Ok(changed)
    }

    /// Makes `id` depend on `predecessor` with the given `dep_type`, per
    /// LLD §Algorithm 2 and §Method Contract, and returns `id`'s task with
    /// the new edge in `depends_on`.
    ///
    /// A task may have any number of predecessors; each new one is appended
    /// after the existing ones. If `id` already depends on `predecessor`,
    /// that edge's type is replaced in place rather than a second edge being
    /// added, so this is also how a caller changes an edge's type.
    ///
    /// Everything runs inside one [`Store::transaction`], and every
    /// rejection returns before anything is written. If `id` already
    /// depends on `predecessor` with this same `dep_type`, nothing is
    /// written and the task is returned as stored, `updated_at` unchanged;
    /// otherwise the edge is added (or its type replaced) and `id`'s
    /// `updated_at` is bumped. `predecessor`'s row is never written.
    ///
    /// # Errors
    ///
    /// - [`CoreError::SelfDependency`] if `id == predecessor`.
    /// - [`CoreError::NotFound`] if `id` or `predecessor` names no live task
    ///   (a soft-deleted one counts as missing); it carries whichever id is
    ///   missing, `id` first.
    /// - [`CoreError::DependsOnRelative`] if `predecessor` is an ancestor or
    ///   a descendant of `id` in the hierarchy.
    /// - [`CoreError::CircularDependency`] if `predecessor` already depends
    ///   on `id`, directly or transitively, through edges of any type and
    ///   through soft-deleted tasks; `cycle` is `[id, predecessor, …, id]`,
    ///   each entry depending on the next.
    /// - [`CoreError::Store`] if the backend fails.
    pub fn add_dependency(
        &mut self,
        id: TaskId,
        predecessor: TaskId,
        dep_type: DependencyType,
    ) -> Result<Task, CoreError> {
        if id == predecessor {
            return Err(CoreError::SelfDependency(id));
        }

        let task = self.store.transaction(|tx| {
            let mut task = match live_task(tx, id) {
                Ok(task) => task,
                Err(e) => return Ok(Err(e)),
            };
            if let Err(e) = live_task(tx, predecessor) {
                return Ok(Err(e));
            }
            if let Err(e) = check_new_dependency(tx, id, predecessor) {
                return Ok(Err(e));
            }
            let unchanged = Dependency {
                predecessor_id: predecessor,
                dep_type,
            };
            if task.depends_on.contains(&unchanged) {
                task.progress = compute_progress(tx, id, task.status)?;
                return Ok(Ok(task));
            }

            tx.add_dependency_edge(predecessor, id, dep_type)?;

            // Read after the edge is written, so `depends_on` — filled by
            // the store from the edges on every read — already includes it.
            let Some(mut task) = tx.get_task(id)? else {
                return Ok(Err(CoreError::NotFound(id)));
            };
            task.progress = compute_progress(tx, id, task.status)?;
            task.updated_at = Utc::now();
            tx.put_task(&task)?;

            Ok(Ok(task))
        })??;

        Ok(task)
    }

    /// Drops `id`'s dependency on `predecessor`, per LLD §Algorithm 2 and
    /// §Method Contract, and returns `id`'s task without that edge in
    /// `depends_on`.
    ///
    /// `predecessor` may be soft-deleted, so a dependency on a deleted task
    /// can still be removed. Removing an edge cannot create a cycle or a
    /// hierarchy conflict, so no invariant check runs.
    ///
    /// Everything runs inside one [`Store::transaction`]. If `id` does not
    /// depend on `predecessor`, nothing is written and the task is returned
    /// as stored, `updated_at` unchanged; otherwise the edge is removed and
    /// `id`'s `updated_at` is bumped. `predecessor`'s row is never written.
    ///
    /// # Errors
    ///
    /// - [`CoreError::NotFound`] if `id` names no live task (a soft-deleted
    ///   one counts as missing).
    /// - [`CoreError::Store`] if the backend fails.
    pub fn remove_dependency(
        &mut self,
        id: TaskId,
        predecessor: TaskId,
    ) -> Result<Task, CoreError> {
        let task = self.store.transaction(|tx| {
            let mut task = match live_task(tx, id) {
                Ok(task) => task,
                Err(e) => return Ok(Err(e)),
            };
            if !task
                .depends_on
                .iter()
                .any(|dep| dep.predecessor_id == predecessor)
            {
                task.progress = compute_progress(tx, id, task.status)?;
                return Ok(Ok(task));
            }

            tx.remove_dependency_edge(predecessor, id)?;

            // Read after the edge is removed, so `depends_on` — filled by
            // the store from the edges on every read — no longer names it.
            let Some(mut task) = tx.get_task(id)? else {
                return Ok(Err(CoreError::NotFound(id)));
            };
            task.progress = compute_progress(tx, id, task.status)?;
            task.updated_at = Utc::now();
            tx.put_task(&task)?;

            Ok(Ok(task))
        })??;

        Ok(task)
    }
}

/// Fetches the live task `id`, or `NotFound`.
fn live_task(tx: &mut dyn StoreTx, id: TaskId) -> Result<Task, CoreError> {
    tx.get_task(id)?.ok_or(CoreError::NotFound(id))
}

/// `parent`'s children in order, keeping `id` and dropping soft-deleted
/// siblings.
fn live_siblings(
    tx: &mut dyn StoreTx,
    parent: Option<TaskId>,
    id: TaskId,
) -> Result<Vec<TaskId>, StoreError> {
    let mut live = Vec::new();
    for sibling in tx.list_child_edges(parent)? {
        if sibling == id || tx.get_task(sibling)?.is_some() {
            live.push(sibling);
        }
    }
    Ok(live)
}

/// Writes `task`'s new parent through the shared cycle-checked writer and
/// persists the updated row (`parent_id`, `updated_at`).
fn write_parent(
    tx: &mut dyn StoreTx,
    task: &mut Task,
    parent: Option<TaskId>,
    placement: Placement,
) -> Result<(), CoreError> {
    hierarchy::set_parent(tx, task.id, parent, placement)?;
    task.parent_id = parent;
    task.updated_at = Utc::now();
    tx.put_task(task)?;
    Ok(())
}

/// Computes `id`'s current progress from its direct children (LLD
/// §Algorithm 4), the single shared entry point every `Task`-returning
/// facade method uses instead of re-deriving a leaf-only placeholder from
/// `status` alone.
///
/// Builds the [`SiblingOrder`] of live tasks from one store call.
fn live_sibling_order(tx: &mut dyn StoreTx) -> Result<SiblingOrder, StoreError> {
    let live: HashSet<TaskId> = tx
        .list_tasks(&TreeFilter::default())?
        .into_iter()
        .map(|t| t.id)
        .collect();
    let mut map: HashMap<Option<TaskId>, Vec<TaskId>> = HashMap::new();
    for (parent, child) in tx.list_all_child_edges()? {
        if live.contains(&child) {
            map.entry(parent).or_default().push(child);
        }
    }
    Ok(map.into())
}

/// Stable-sorts `tasks` into depth-first sibling order (roots first, each
/// task followed by its children). Tasks not reached (e.g. under a deleted
/// parent) keep their relative order at the end.
fn sort_by_hierarchy_order(tx: &mut dyn StoreTx, tasks: &mut [Task]) -> Result<(), StoreError> {
    let order = live_sibling_order(tx)?;
    let mut rank: HashMap<TaskId, usize> = HashMap::new();
    let mut stack: Vec<TaskId> = order.children_of(None).iter().rev().copied().collect();
    while let Some(id) = stack.pop() {
        if rank.contains_key(&id) {
            continue;
        }
        rank.insert(id, rank.len());
        stack.extend(order.children_of(Some(id)).iter().rev().copied());
    }
    tasks.sort_by_key(|t| rank.get(&t.id).copied().unwrap_or(usize::MAX));
    Ok(())
}

/// Fetches `id`'s direct children the same way `get_tree`/`list_children`
/// already do (`StoreTx::list_child_edges` + `StoreTx::get_task`, skipping
/// any child id that no longer resolves) and hands them to
/// `rollup::direct_children_progress` along with `status` — `id`'s own
/// current status, passed in rather than re-fetched, since every call site
/// already has it in hand from the row it just read or is about to write.
fn compute_progress(
    tx: &mut dyn StoreTx,
    id: TaskId,
    status: TaskStatus,
) -> Result<f32, StoreError> {
    let children = tx
        .list_child_edges(Some(id))?
        .into_iter()
        .filter_map(|child_id| tx.get_task(child_id).transpose())
        .collect::<Result<Vec<Task>, StoreError>>()?;
    Ok(rollup::direct_children_progress(&children, status))
}

/// Collects the ids of every descendant of `id`, at any depth, whose
/// `status` is still [`TaskStatus::Incomplete`] — the same set
/// [`mark_complete_subtree`] would flip to [`TaskStatus::Complete`] under
/// `cascade: true`, so `Core::complete_task`'s rejection always names
/// exactly what committing would touch.
///
/// Iterative (an explicit work stack), not recursive, for the same
/// deep-chain-safety reason as `mark_complete_subtree`/`tombstone_subtree`.
/// Each task has one parent, so the walk reaches every descendant exactly
/// once and none is counted twice. A child id that no longer resolves via
/// [`StoreTx::get_task`] (already deleted) is skipped, matching how other
/// code in this module treats missing lookups.
fn incomplete_descendants(tx: &mut dyn StoreTx, id: TaskId) -> Result<Vec<TaskId>, StoreError> {
    let mut incomplete = Vec::new();
    let mut pending = tx.list_child_edges(Some(id))?;

    while let Some(current) = pending.pop() {
        let Some(task) = tx.get_task(current)? else {
            continue;
        };
        if task.status == TaskStatus::Incomplete {
            incomplete.push(current);
        }
        pending.extend(tx.list_child_edges(Some(current))?);
    }

    Ok(incomplete)
}

/// Marks `id` and its entire subtree complete, for `Core::complete_task`
/// under `cascade = true`.
///
/// Iterative (an explicit work stack), not recursive, for the same
/// deep-chain-safety reason as `tombstone_subtree`. Unlike that function,
/// this never touches parent/child edges — it only walks descendants via
/// [`StoreTx::list_child_edges`] and flips `status`/`completed_at`/
/// `updated_at`. Each task has one parent, so the walk reaches every
/// descendant exactly once. A task already [`TaskStatus::Complete`] is
/// walked through (to reach further descendants) but not re-written or
/// added to `touched`, so its `completed_at` and `updated_at` are kept and
/// it isn't reported as changed.
///
/// Two passes, deliberately: the first flips every not-yet-complete
/// descendant's `status`/`completed_at`/`updated_at` and commits it, without
/// touching `progress` at all; only once every status in the subtree has
/// reached its final value does a second pass compute each touched task's
/// `progress` via [`compute_progress`]. A single combined pass would read a
/// wrong, stale `progress` for any task processed before its own children —
/// the work-stack order here visits `id` itself before descending into its
/// children, so `compute_progress(tx, id, ...)` at that point would see
/// children whose `status` hadn't been flipped to `Complete` yet, silently
/// under-reporting `id`'s rolled-up progress even though the whole subtree
/// ends up `Complete` by the time this function returns.
fn mark_complete_subtree(
    tx: &mut dyn StoreTx,
    id: TaskId,
    now: chrono::DateTime<Utc>,
    touched: &mut Vec<Task>,
) -> Result<(), StoreError> {
    let mut pending = vec![id];
    let mut touched_ids = Vec::new();

    while let Some(current) = pending.pop() {
        let Some(mut task) = tx.get_task(current)? else {
            continue;
        };

        if task.status != TaskStatus::Complete {
            task.status = TaskStatus::Complete;
            task.completed_at = Some(now);
            task.updated_at = now;
            tx.put_task(&task)?;
            touched_ids.push(current);
        }

        pending.extend(tx.list_child_edges(Some(current))?);
    }

    for touched_id in touched_ids {
        let Some(mut task) = tx.get_task(touched_id)? else {
            continue;
        };
        task.progress = compute_progress(tx, touched_id, task.status)?;
        tx.put_task(&task)?;
        touched.push(task);
    }

    Ok(())
}

/// Tombstones `id` and every descendant under [`DeleteMode::Subtree`]:
/// each gets `deleted_at`/`updated_at` set and is appended to `deleted`,
/// once. A task has one parent, so everything beneath `id` goes with it and
/// no task outside the subtree is touched.
///
/// Each tombstoned task's children have their parent edge moved to the top
/// level as the walk passes, so a tombstoned task is left with no child
/// edges; `id`'s own parent edge is kept.
///
/// Iterative (an explicit work stack), not recursive: `create_task`
/// permits arbitrarily deep parent chains, so a user-built hierarchy deep
/// enough could overflow the process stack if this walked it via Rust
/// call recursion instead.
fn tombstone_subtree(
    tx: &mut dyn StoreTx,
    id: TaskId,
    now: chrono::DateTime<Utc>,
    deleted: &mut Vec<Task>,
) -> Result<(), StoreError> {
    let mut pending = vec![id];

    while let Some(current) = pending.pop() {
        let Some(mut task) = tx.get_task_including_deleted(current)? else {
            continue;
        };
        // A task tombstoned by an earlier call keeps its own parent edge,
        // so it's still among its parent's child edges and gets queued
        // here. It already has no child edges of its own; re-tombstoning
        // it would overwrite its original `deleted_at` and wrongly report
        // it as deleted by this call.
        if task.deleted_at.is_some() {
            continue;
        }
        task.deleted_at = Some(now);
        task.updated_at = now;

        for child in tx.list_child_edges(Some(current))? {
            tx.set_parent_edge(child, None, Placement::End)?;
            pending.push(child);
        }

        // `status` is untouched by a delete, but `progress` is re-derived
        // rather than trusted from the fetched row — see `update_task`'s
        // matching comment. Computed here, after the loop above has moved
        // every one of `current`'s children away, so it reflects the
        // committed (now-empty) child-edge set — matching what a later
        // independent fetch (e.g. `get_tree` with `include_deleted: true`)
        // would recompute, rather than a stale pre-delete snapshot the
        // returned `Task` can no longer back up.
        task.progress = compute_progress(tx, current, task.status)?;
        tx.put_task(&task)?;
        deleted.push(task);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::in_memory_store::InMemoryStore;
    use crate::model::TaskStatus;
    use crate::test_support::CountingStore;
    use chrono::NaiveDate;

    fn new_core() -> Core<InMemoryStore> {
        Core::new(InMemoryStore::default()).unwrap()
    }

    fn minimal_new_task(title: &str) -> NewTask {
        NewTask {
            title: title.to_owned(),
            description: None,
            parent_id: None,
            type_key: None,
            start_date: None,
            due_date: None,
            assignee_id: None,
        }
    }

    #[test]
    fn create_task_should_reject_empty_title() {
        let mut core = new_core();

        let result = core.create_task(minimal_new_task(""));

        assert!(matches!(result, Err(CoreError::EmptyTitle)));
    }

    #[test]
    fn create_task_should_reject_whitespace_only_title() {
        let mut core = new_core();

        let result = core.create_task(minimal_new_task("   \t  "));

        assert!(matches!(result, Err(CoreError::EmptyTitle)));
    }

    #[test]
    fn create_task_should_assign_unique_id_and_created_at() {
        let mut core = new_core();

        let a = core.create_task(minimal_new_task("Task A")).unwrap();
        let b = core.create_task(minimal_new_task("Task B")).unwrap();

        assert_ne!(a.id, b.id);
        assert_eq!(a.created_at, a.updated_at);
        assert_eq!(b.created_at, b.updated_at);
    }

    #[test]
    fn create_task_should_default_to_top_level_when_no_parent_given() {
        let mut core = new_core();

        let task = core.create_task(minimal_new_task("Top level")).unwrap();

        assert_eq!(task.parent_id, None);
    }

    #[test]
    fn create_task_should_attach_to_given_parent() {
        let mut core = new_core();
        let parent = core.create_task(minimal_new_task("Parent")).unwrap();

        let child = core
            .create_task(NewTask {
                parent_id: Some(parent.id),
                ..minimal_new_task("Child")
            })
            .unwrap();

        assert_eq!(child.parent_id, Some(parent.id));
        // The one stored edge names the parent, and the child is not also
        // at the top level.
        let stored_parent = core
            .store
            .transaction(|tx| tx.get_parent_edge(child.id))
            .unwrap();
        assert_eq!(stored_parent, Some(parent.id));
        let order = core.sibling_order().unwrap();
        assert_eq!(order.children_of(Some(parent.id)), [child.id]);
        assert_eq!(order.children_of(None), [parent.id]);
    }

    #[test]
    fn create_task_should_reject_when_given_parent_does_not_exist() {
        let mut core = new_core();
        let missing_parent = TaskId::new();

        let result = core.create_task(NewTask {
            parent_id: Some(missing_parent),
            ..minimal_new_task("Orphan")
        });

        assert!(matches!(result, Err(CoreError::NotFound(id)) if id == missing_parent));
    }

    #[test]
    fn create_task_should_default_type_key_to_task_when_unset() {
        let mut core = new_core();

        let task = core.create_task(minimal_new_task("Untyped")).unwrap();

        assert_eq!(task.type_key, "task");
    }

    #[test]
    fn create_task_should_reject_unknown_type_key() {
        let mut core = new_core();

        let result = core.create_task(NewTask {
            type_key: Some("bogus".to_owned()),
            ..minimal_new_task("Mistyped")
        });

        assert!(matches!(result, Err(CoreError::UnknownTaskType(key)) if key == "bogus"));
    }

    #[test]
    fn create_task_should_store_optional_description_and_dates() {
        let mut core = new_core();
        let start = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let due = NaiveDate::from_ymd_opt(2026, 1, 31).unwrap();

        let task = core
            .create_task(NewTask {
                description: Some("Details".to_owned()),
                start_date: Some(start),
                due_date: Some(due),
                ..minimal_new_task("With details")
            })
            .unwrap();

        assert_eq!(task.description.as_deref(), Some("Details"));
        assert_eq!(task.start_date, Some(start));
        assert_eq!(task.due_date, Some(due));
    }

    #[test]
    fn create_task_should_reject_due_date_before_start_date() {
        let mut core = new_core();
        let start = NaiveDate::from_ymd_opt(2026, 1, 31).unwrap();
        let due = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();

        let result = core.create_task(NewTask {
            start_date: Some(start),
            due_date: Some(due),
            ..minimal_new_task("Backwards dates")
        });

        assert!(matches!(
            result,
            Err(CoreError::InvalidDateRange { start: s, due: d }) if s == start && d == due
        ));
    }

    /// A `NewTask` that fails every check past the title: unknown type,
    /// backwards dates, a missing parent, and an unknown assignee.
    fn new_task_failing_every_check() -> NewTask {
        NewTask {
            type_key: Some("bogus".to_owned()),
            start_date: NaiveDate::from_ymd_opt(2026, 1, 31),
            due_date: NaiveDate::from_ymd_opt(2026, 1, 1),
            parent_id: Some(TaskId::new()),
            assignee_id: Some(UserId::new()),
            ..minimal_new_task("Everything wrong")
        }
    }

    /// An unknown type key wins over every later failure.
    #[test]
    fn create_task_should_report_unknown_type_before_invalid_date_range() {
        let mut core = new_core();

        let result = core.create_task(new_task_failing_every_check());

        assert!(matches!(result, Err(CoreError::UnknownTaskType(key)) if key == "bogus"));
    }

    /// A backwards date range wins over a missing parent or assignee.
    #[test]
    fn create_task_should_report_invalid_date_range_before_missing_parent() {
        let mut core = new_core();

        let result = core.create_task(NewTask {
            type_key: None,
            ..new_task_failing_every_check()
        });

        assert!(matches!(result, Err(CoreError::InvalidDateRange { .. })));
    }

    /// A missing parent wins over an unknown assignee.
    #[test]
    fn create_task_should_report_missing_parent_before_unknown_assignee() {
        let mut core = new_core();
        let failing = NewTask {
            type_key: None,
            start_date: None,
            due_date: None,
            ..new_task_failing_every_check()
        };
        let missing = failing.parent_id.unwrap();

        let result = core.create_task(failing);

        assert!(matches!(result, Err(CoreError::NotFound(id)) if id == missing));
    }

    #[test]
    fn create_task_should_make_the_same_store_calls_at_any_parent_depth() {
        let store = CountingStore::new();
        let log = store.log();
        let calls = || log.borrow().len();
        let mut core = Core::new(store).unwrap();
        let top = core.create_task(minimal_new_task("Top")).unwrap();
        let mut deepest = top.id;
        for i in 0..50 {
            deepest = core
                .create_task(NewTask {
                    parent_id: Some(deepest),
                    ..minimal_new_task(&format!("Level {i}"))
                })
                .unwrap()
                .id;
        }

        let before_shallow = calls();
        core.create_task(NewTask {
            parent_id: Some(top.id),
            ..minimal_new_task("Under top")
        })
        .unwrap();
        let shallow_calls = calls() - before_shallow;

        let before_deep = calls();
        core.create_task(NewTask {
            parent_id: Some(deepest),
            ..minimal_new_task("Under deepest")
        })
        .unwrap();
        let deep_calls = calls() - before_deep;

        assert_eq!(deep_calls, shallow_calls);
    }

    /// A soft-deleted parent is reported as `NotFound`, like a missing one.
    #[test]
    fn create_task_should_reject_soft_deleted_parent() {
        let mut core = new_core();
        let parent = core.create_task(minimal_new_task("Parent")).unwrap();
        core.delete_task(parent.id, DeleteMode::Subtree).unwrap();

        let result = core.create_task(NewTask {
            parent_id: Some(parent.id),
            ..minimal_new_task("Child")
        });

        assert!(matches!(result, Err(CoreError::NotFound(id)) if id == parent.id));
    }

    /// Every task (deleted included) and every parent edge in `core`'s
    /// store, for asserting that a rejected call wrote nothing.
    fn store_snapshot(core: &Core<InMemoryStore>) -> (Vec<Task>, Vec<(Option<TaskId>, TaskId)>) {
        let all = TreeFilter {
            include_deleted: true,
            ..TreeFilter::default()
        };
        core.store
            .transaction(|tx| Ok((tx.list_tasks(&all)?, tx.list_all_child_edges()?)))
            .unwrap()
    }

    /// An unknown type key leaves the store untouched.
    #[test]
    fn create_task_should_write_nothing_when_type_key_unknown() {
        let mut core = new_core();
        let before = store_snapshot(&core);

        let result = core.create_task(NewTask {
            type_key: Some("bogus".to_owned()),
            ..minimal_new_task("Mistyped")
        });

        assert!(matches!(result, Err(CoreError::UnknownTaskType(key)) if key == "bogus"));
        assert_eq!(store_snapshot(&core), before);
    }

    /// An unknown assignee leaves the store untouched.
    #[test]
    fn create_task_should_write_nothing_when_assignee_unknown() {
        let mut core = new_core();
        let unknown_assignee = UserId::new();
        let before = store_snapshot(&core);

        let result = core.create_task(NewTask {
            assignee_id: Some(unknown_assignee),
            ..minimal_new_task("Orphan assignee")
        });

        assert!(matches!(result, Err(CoreError::UnknownUser(id)) if id == unknown_assignee));
        assert_eq!(store_snapshot(&core), before);
    }

    /// A parent that is missing or soft-deleted leaves the store untouched:
    /// no task row and no parent edge.
    #[test]
    fn create_task_should_write_nothing_when_the_parent_is_missing() {
        let mut core = new_core();
        let missing_parent = TaskId::new();
        let deleted_parent = core.create_task(minimal_new_task("Deleted")).unwrap();
        core.delete_task(deleted_parent.id, DeleteMode::Subtree)
            .unwrap();
        let before = store_snapshot(&core);

        let missing = core.create_task(NewTask {
            parent_id: Some(missing_parent),
            ..minimal_new_task("Under missing")
        });
        let deleted = core.create_task(NewTask {
            parent_id: Some(deleted_parent.id),
            ..minimal_new_task("Under deleted")
        });

        assert!(matches!(missing, Err(CoreError::NotFound(id)) if id == missing_parent));
        assert!(matches!(deleted, Err(CoreError::NotFound(id)) if id == deleted_parent.id));
        assert_eq!(store_snapshot(&core), before);
    }

    /// A create with a type, a parent, and an assignee makes every store
    /// call in one transaction.
    #[test]
    fn create_task_should_run_in_a_single_transaction() {
        let store = CountingStore::new();
        let log = store.log();
        let mut core = Core::new(store).unwrap();
        core.upsert_task_type(TaskType {
            key: "goal".to_owned(),
            label: "Goal".to_owned(),
            color: None,
            sort_order: 1,
        })
        .unwrap();
        let parent = core.create_task(minimal_new_task("Parent")).unwrap();
        let assignee = core.create_user("Ada".to_owned()).unwrap();
        let start = log.borrow().len();

        core.create_task(NewTask {
            type_key: Some("goal".to_owned()),
            parent_id: Some(parent.id),
            assignee_id: Some(assignee.id),
            ..minimal_new_task("Child")
        })
        .unwrap();

        let calls = log.borrow()[start..].to_vec();
        let mut txs: Vec<usize> = calls.iter().map(|&(tx, _)| tx).collect();
        txs.dedup();
        assert_eq!(txs.len(), 1, "calls: {calls:?}");
    }

    #[test]
    fn get_tree_should_reflect_updated_child_completion_immediately() {
        let mut core = new_core();
        let parent = core.create_task(minimal_new_task("Parent")).unwrap();
        let child_a = core
            .create_task(NewTask {
                parent_id: Some(parent.id),
                ..minimal_new_task("Child A")
            })
            .unwrap();
        let _child_b = core
            .create_task(NewTask {
                parent_id: Some(parent.id),
                ..minimal_new_task("Child B")
            })
            .unwrap();

        let tree_before = core.get_tree(TreeFilter::default()).unwrap();
        let parent_before = tree_before.iter().find(|t| t.id == parent.id).unwrap();
        assert!((parent_before.progress - 0.0).abs() < f32::EPSILON);

        core.complete_task(child_a.id, false).unwrap();

        let tree_after = core.get_tree(TreeFilter::default()).unwrap();
        let parent_after = tree_after.iter().find(|t| t.id == parent.id).unwrap();
        assert!((parent_after.progress - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn get_tree_should_include_a_just_created_task() {
        let mut core = new_core();

        let created = core.create_task(minimal_new_task("Findable")).unwrap();
        let tree = core.get_tree(TreeFilter::default()).unwrap();

        assert!(tree.contains(&created));
    }

    #[test]
    fn new_should_seed_default_task_type() {
        let core = new_core();

        let types = core.list_task_types().unwrap();

        assert!(types.iter().any(|t| t.key == "task"));
    }

    #[test]
    fn new_is_idempotent_about_seeding_the_default_task_type() {
        let store = InMemoryStore::default();
        let core_a = Core::new(store).unwrap();
        let types_after_first = core_a.list_task_types().unwrap();
        assert_eq!(
            types_after_first.iter().filter(|t| t.key == "task").count(),
            1
        );

        // Re-seeding over a store that already has the default leaves it
        // as a single entry, not a duplicate.
        let store_with_default = InMemoryStore::default();
        store_with_default
            .transaction(|tx| {
                tx.put_task_type(&TaskType {
                    key: "task".to_owned(),
                    label: "Task".to_owned(),
                    color: None,
                    sort_order: 0,
                })
            })
            .unwrap();
        let core_b = Core::new(store_with_default).unwrap();
        let types_after_second = core_b.list_task_types().unwrap();
        assert_eq!(
            types_after_second
                .iter()
                .filter(|t| t.key == "task")
                .count(),
            1
        );
    }

    #[test]
    fn list_task_types_and_upsert_task_type_round_trip() {
        let mut core = new_core();
        let goal = TaskType {
            key: "goal".to_owned(),
            label: "Goal".to_owned(),
            color: None,
            sort_order: 1,
        };

        let upserted = core.upsert_task_type(goal.clone()).unwrap();
        let types = core.list_task_types().unwrap();

        assert_eq!(upserted, goal);
        assert!(types.contains(&goal));
    }

    #[test]
    fn create_task_uses_status_incomplete() {
        let mut core = new_core();

        let task = core.create_task(minimal_new_task("New")).unwrap();

        assert_eq!(task.status, TaskStatus::Incomplete);
    }

    #[test]
    fn create_task_should_set_progress_zero_for_new_incomplete_task() {
        let mut core = new_core();

        let task = core.create_task(minimal_new_task("New")).unwrap();

        assert!((task.progress - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn create_user_should_reject_empty_name() {
        let mut core = new_core();

        let result = core.create_user(String::new());

        assert!(matches!(result, Err(CoreError::EmptyUserName)));
    }

    #[test]
    fn create_user_should_reject_whitespace_only_name() {
        let mut core = new_core();

        let result = core.create_user("   \t  ".to_owned());

        assert!(matches!(result, Err(CoreError::EmptyUserName)));
    }

    #[test]
    fn create_user_should_assign_unique_id() {
        let mut core = new_core();

        let a = core.create_user("Ada".to_owned()).unwrap();
        let b = core.create_user("Grace".to_owned()).unwrap();

        assert_ne!(a.id, b.id);
    }

    #[test]
    fn list_users_should_include_a_just_created_user() {
        let mut core = new_core();

        let created = core.create_user("Findable".to_owned()).unwrap();
        let users = core.list_users().unwrap();

        assert!(users.contains(&created));
    }

    #[test]
    fn create_task_should_store_given_assignee_id() {
        let mut core = new_core();
        let user = core.create_user("Ada".to_owned()).unwrap();

        let task = core
            .create_task(NewTask {
                assignee_id: Some(user.id),
                ..minimal_new_task("Assigned")
            })
            .unwrap();

        assert_eq!(task.assignee_id, Some(user.id));
    }

    #[test]
    fn create_task_should_default_assignee_to_none_when_unset() {
        let mut core = new_core();

        let task = core.create_task(minimal_new_task("Unassigned")).unwrap();

        assert!(task.assignee_id.is_none());
    }

    #[test]
    fn create_task_should_reject_unknown_assignee_id() {
        let mut core = new_core();
        let unknown_assignee = UserId::new();

        let result = core.create_task(NewTask {
            assignee_id: Some(unknown_assignee),
            ..minimal_new_task("Orphan assignee")
        });

        assert!(matches!(result, Err(CoreError::UnknownUser(id)) if id == unknown_assignee));
    }

    #[test]
    fn get_tree_should_filter_by_assignee_id() {
        let mut core = new_core();
        let alice = core.create_user("Alice".to_owned()).unwrap();
        let bob = core.create_user("Bob".to_owned()).unwrap();

        let alice_task = core
            .create_task(NewTask {
                assignee_id: Some(alice.id),
                ..minimal_new_task("Alice's task")
            })
            .unwrap();
        core.create_task(NewTask {
            assignee_id: Some(bob.id),
            ..minimal_new_task("Bob's task")
        })
        .unwrap();

        let tree = core
            .get_tree(TreeFilter {
                assignee_id: Some(alice.id),
                ..TreeFilter::default()
            })
            .unwrap();

        assert_eq!(tree, vec![alice_task]);
    }

    #[test]
    fn update_task_should_reject_when_task_not_found() {
        let mut core = new_core();
        let missing = TaskId::new();

        let result = core.update_task(missing, TaskPatch::default());

        assert!(matches!(result, Err(CoreError::NotFound(id)) if id == missing));
    }

    #[test]
    fn update_task_should_update_title_when_set() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Original")).unwrap();

        let updated = core
            .update_task(
                task.id,
                TaskPatch {
                    title: Field::Set("Renamed".to_owned()),
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(updated[0].title, "Renamed");
    }

    #[test]
    fn update_task_should_leave_title_unchanged_when_kept() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Original")).unwrap();

        let updated = core.update_task(task.id, TaskPatch::default()).unwrap();

        assert_eq!(updated[0].title, "Original");
    }

    #[test]
    fn update_task_should_reject_empty_title() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Original")).unwrap();

        let result = core.update_task(
            task.id,
            TaskPatch {
                title: Field::Set(String::new()),
                ..Default::default()
            },
        );

        assert!(matches!(result, Err(CoreError::EmptyTitle)));
    }

    #[test]
    fn update_task_should_reject_whitespace_only_title() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Original")).unwrap();

        let result = core.update_task(
            task.id,
            TaskPatch {
                title: Field::Set("   \t  ".to_owned()),
                ..Default::default()
            },
        );

        assert!(matches!(result, Err(CoreError::EmptyTitle)));
    }

    #[test]
    fn update_task_should_set_and_clear_description() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Task")).unwrap();

        let updated = core
            .update_task(
                task.id,
                TaskPatch {
                    description: Field::Set("Details".to_owned()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated[0].description.as_deref(), Some("Details"));

        let cleared = core
            .update_task(
                task.id,
                TaskPatch {
                    description: Field::Clear,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(cleared[0].description, None);
    }

    #[test]
    fn update_task_should_set_and_clear_start_and_due_dates() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Task")).unwrap();
        let start = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let due = NaiveDate::from_ymd_opt(2026, 1, 31).unwrap();

        let updated = core
            .update_task(
                task.id,
                TaskPatch {
                    start_date: Field::Set(start),
                    due_date: Field::Set(due),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated[0].start_date, Some(start));
        assert_eq!(updated[0].due_date, Some(due));

        let cleared = core
            .update_task(
                task.id,
                TaskPatch {
                    start_date: Field::Clear,
                    due_date: Field::Clear,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(cleared[0].start_date, None);
        assert_eq!(cleared[0].due_date, None);
    }

    #[test]
    fn update_task_should_reject_due_date_before_start_date_after_patch() {
        let mut core = new_core();
        let start = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let earlier_due = NaiveDate::from_ymd_opt(2025, 12, 1).unwrap();
        let task = core
            .create_task(NewTask {
                start_date: Some(start),
                ..minimal_new_task("Task")
            })
            .unwrap();

        let result = core.update_task(
            task.id,
            TaskPatch {
                due_date: Field::Set(earlier_due),
                ..Default::default()
            },
        );

        assert!(matches!(
            result,
            Err(CoreError::InvalidDateRange { start: s, due: d }) if s == start && d == earlier_due
        ));
    }

    #[test]
    fn update_task_should_set_and_clear_assignee() {
        let mut core = new_core();
        let user = core.create_user("Ada".to_owned()).unwrap();
        let task = core.create_task(minimal_new_task("Task")).unwrap();

        let updated = core
            .update_task(
                task.id,
                TaskPatch {
                    assignee_id: Field::Set(user.id),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated[0].assignee_id, Some(user.id));

        let cleared = core
            .update_task(
                task.id,
                TaskPatch {
                    assignee_id: Field::Clear,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(cleared[0].assignee_id, None);
    }

    #[test]
    fn update_task_should_reject_unknown_assignee_id() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Task")).unwrap();
        let unknown_assignee = UserId::new();

        let result = core.update_task(
            task.id,
            TaskPatch {
                assignee_id: Field::Set(unknown_assignee),
                ..Default::default()
            },
        );

        assert!(matches!(result, Err(CoreError::UnknownUser(id)) if id == unknown_assignee));
    }

    #[test]
    fn update_task_should_update_type_key_when_set() {
        let mut core = new_core();
        let goal = TaskType {
            key: "goal".to_owned(),
            label: "Goal".to_owned(),
            color: None,
            sort_order: 1,
        };
        core.upsert_task_type(goal).unwrap();
        let task = core.create_task(minimal_new_task("Task")).unwrap();

        let updated = core
            .update_task(
                task.id,
                TaskPatch {
                    type_key: Field::Set("goal".to_owned()),
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(updated[0].type_key, "goal");
    }

    #[test]
    fn update_task_should_leave_dates_unchanged_when_only_type_key_changes() {
        let mut core = new_core();
        let goal = TaskType {
            key: "goal".to_owned(),
            label: "Goal".to_owned(),
            color: None,
            sort_order: 1,
        };
        core.upsert_task_type(goal).unwrap();
        let start = NaiveDate::from_ymd_opt(2026, 3, 1).unwrap();
        let due = NaiveDate::from_ymd_opt(2026, 3, 15).unwrap();
        let task = core
            .create_task(NewTask {
                start_date: Some(start),
                due_date: Some(due),
                ..minimal_new_task("Task")
            })
            .unwrap();

        let updated = core
            .update_task(
                task.id,
                TaskPatch {
                    type_key: Field::Set("goal".to_owned()),
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(updated[0].type_key, "goal");
        assert_eq!(updated[0].start_date, Some(start));
        assert_eq!(updated[0].due_date, Some(due));
    }

    #[test]
    fn update_task_should_leave_parent_id_unchanged_when_only_type_key_changes() {
        let mut core = new_core();
        let goal = TaskType {
            key: "goal".to_owned(),
            label: "Goal".to_owned(),
            color: None,
            sort_order: 1,
        };
        core.upsert_task_type(goal).unwrap();
        let parent = core.create_task(minimal_new_task("Parent")).unwrap();
        let child = core
            .create_task(NewTask {
                parent_id: Some(parent.id),
                ..minimal_new_task("Child")
            })
            .unwrap();

        let updated = core
            .update_task(
                child.id,
                TaskPatch {
                    type_key: Field::Set("goal".to_owned()),
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(updated[0].type_key, "goal");
        assert_eq!(updated[0].parent_id, Some(parent.id));
    }

    #[test]
    fn update_task_should_reject_unknown_type_key() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Task")).unwrap();

        let result = core.update_task(
            task.id,
            TaskPatch {
                type_key: Field::Set("bogus".to_owned()),
                ..Default::default()
            },
        );

        assert!(matches!(result, Err(CoreError::UnknownTaskType(key)) if key == "bogus"));
    }

    #[test]
    fn update_task_should_bump_updated_at_but_not_created_at() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Task")).unwrap();

        let updated = core
            .update_task(
                task.id,
                TaskPatch {
                    title: Field::Set("Renamed".to_owned()),
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(updated[0].created_at, task.created_at);
        assert!(updated[0].updated_at >= task.updated_at);
    }

    #[test]
    fn update_task_should_not_change_subtasks_dates() {
        let mut core = new_core();
        let parent = core.create_task(minimal_new_task("Parent")).unwrap();
        let child_start = NaiveDate::from_ymd_opt(2026, 2, 1).unwrap();
        let child_due = NaiveDate::from_ymd_opt(2026, 2, 28).unwrap();
        let child = core
            .create_task(NewTask {
                parent_id: Some(parent.id),
                start_date: Some(child_start),
                due_date: Some(child_due),
                ..minimal_new_task("Child")
            })
            .unwrap();

        let new_start = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let new_due = NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();
        core.update_task(
            parent.id,
            TaskPatch {
                start_date: Field::Set(new_start),
                due_date: Field::Set(new_due),
                ..Default::default()
            },
        )
        .unwrap();

        let tree = core.get_tree(TreeFilter::default()).unwrap();
        let child_after = tree.iter().find(|t| t.id == child.id).unwrap();
        assert_eq!(child_after.start_date, Some(child_start));
        assert_eq!(child_after.due_date, Some(child_due));
    }

    #[test]
    fn update_task_should_return_only_the_updated_task() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Task")).unwrap();

        let result = core
            .update_task(
                task.id,
                TaskPatch {
                    title: Field::Set("Renamed".to_owned()),
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].title, "Renamed");
    }

    #[test]
    fn delete_task_should_reject_when_task_not_found() {
        let mut core = new_core();
        let missing = TaskId::new();

        let result = core.delete_task(missing, DeleteMode::Subtree);

        assert!(matches!(result, Err(CoreError::NotFound(id)) if id == missing));
    }

    #[test]
    fn delete_task_should_tombstone_leaf_task() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Leaf")).unwrap();

        let outcome = core.delete_task(task.id, DeleteMode::Subtree).unwrap();

        assert_eq!(outcome.deleted.len(), 1);
        assert_eq!(outcome.deleted[0].id, task.id);
        assert!(outcome.deleted[0].deleted_at.is_some());
        assert_eq!(outcome.updated, []);

        // A second delete on the same (now soft-deleted) task is treated
        // as not-found, not re-tombstoned.
        let result = core.delete_task(task.id, DeleteMode::Subtree);
        assert!(matches!(result, Err(CoreError::NotFound(id)) if id == task.id));
    }

    /// A branching tree to delete from: `root` sits under `above` beside
    /// `beside`, and holds `descendants` across three levels.
    struct SubtreeFixture {
        above: TaskId,
        beside: TaskId,
        root: TaskId,
        descendants: Vec<TaskId>,
    }

    fn subtree_fixture(core: &mut Core<InMemoryStore>) -> SubtreeFixture {
        let above = titled_under(core, "Above", None).id;
        let root = titled_under(core, "Root", Some(above)).id;
        let beside = titled_under(core, "Beside", Some(above)).id;
        let a = titled_under(core, "A", Some(root)).id;
        let b = titled_under(core, "B", Some(root)).id;
        let a1 = titled_under(core, "A1", Some(a)).id;
        let a2 = titled_under(core, "A2", Some(a)).id;
        let deepest = titled_under(core, "Deepest", Some(a2)).id;
        SubtreeFixture {
            above,
            beside,
            root,
            descendants: vec![a, b, a1, a2, deepest],
        }
    }

    #[test]
    fn delete_task_subtree_should_tombstone_every_descendant() {
        let mut core = new_core();
        let fixture = subtree_fixture(&mut core);

        let outcome = core.delete_task(fixture.root, DeleteMode::Subtree).unwrap();

        let expected: HashSet<_> = fixture
            .descendants
            .iter()
            .copied()
            .chain([fixture.root])
            .collect();
        let deleted_ids: HashSet<_> = outcome.deleted.iter().map(|t| t.id).collect();
        assert_eq!(deleted_ids, expected);
        assert_eq!(outcome.deleted.len(), expected.len());
        assert!(outcome.deleted.iter().all(|t| t.deleted_at.is_some()));

        // The store agrees: nothing in the subtree is still live, and
        // nothing outside it was tombstoned.
        let live: HashSet<_> = core
            .get_tree(TreeFilter::default())
            .unwrap()
            .iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(live, HashSet::from([fixture.above, fixture.beside]));
    }

    #[test]
    fn delete_task_subtree_should_leave_updated_empty() {
        let mut core = new_core();
        let fixture = subtree_fixture(&mut core);
        let above_before = core.get_task(fixture.above).unwrap().unwrap();
        let beside_before = core.get_task(fixture.beside).unwrap().unwrap();

        let outcome = core.delete_task(fixture.root, DeleteMode::Subtree).unwrap();

        assert_eq!(outcome.updated, []);
        // The tasks around the subtree are reported in neither list and
        // their stored rows are untouched.
        assert!(
            outcome
                .deleted
                .iter()
                .all(|t| t.id != fixture.above && t.id != fixture.beside)
        );
        assert_eq!(core.get_task(fixture.above).unwrap(), Some(above_before));
        assert_eq!(core.get_task(fixture.beside).unwrap(), Some(beside_before));
    }

    #[test]
    fn delete_task_subtree_should_return_progress_matching_a_later_get_tree_fetch() {
        // Regression test: `tombstone_subtree` used to compute a
        // tombstoned task's `progress` from its live children *before*
        // severing its child edges, so the value in `delete_task`'s own
        // returned `DeleteOutcome` didn't match what a later independent
        // fetch (which always recomputes live, from whatever children
        // edges are left after the delete) would report. One Complete
        // and one Incomplete child makes the two states unambiguously
        // different: pre-severance progress is 0.5 (one of two direct
        // children complete); post-severance (both children's edges to
        // `parent` are gone) it falls back to `parent`'s own Incomplete
        // status, 0.0 — so a stale pre-severance return value is
        // distinguishable from the correct, post-severance one.
        let mut core = new_core();
        let parent = core.create_task(minimal_new_task("Parent")).unwrap();
        let complete_child = core
            .create_task(NewTask {
                parent_id: Some(parent.id),
                ..minimal_new_task("Complete child")
            })
            .unwrap();
        core.complete_task(complete_child.id, false).unwrap();
        core.create_task(NewTask {
            parent_id: Some(parent.id),
            ..minimal_new_task("Incomplete child")
        })
        .unwrap();

        let outcome = core.delete_task(parent.id, DeleteMode::Subtree).unwrap();
        let returned_parent = outcome.deleted.iter().find(|t| t.id == parent.id).unwrap();

        let tree = core
            .get_tree(TreeFilter {
                include_deleted: true,
                ..TreeFilter::default()
            })
            .unwrap();
        let refetched_parent = tree.iter().find(|t| t.id == parent.id).unwrap();

        assert!((returned_parent.progress - refetched_parent.progress).abs() < f32::EPSILON);
    }

    #[test]
    fn delete_task_promote_children_should_return_progress_matching_a_later_get_tree_fetch() {
        // Same regression as the `Subtree` variant above, for
        // `DeleteMode::PromoteChildren`'s `deleted.progress` computation:
        // one Complete and one Incomplete child so pre-reparent (0.5) and
        // post-reparent (0.0, own-status fallback once both children are
        // reparented away) values are distinguishable.
        let mut core = new_core();
        let parent = core.create_task(minimal_new_task("Parent")).unwrap();
        let complete_child = core
            .create_task(NewTask {
                parent_id: Some(parent.id),
                ..minimal_new_task("Complete child")
            })
            .unwrap();
        core.complete_task(complete_child.id, false).unwrap();
        core.create_task(NewTask {
            parent_id: Some(parent.id),
            ..minimal_new_task("Incomplete child")
        })
        .unwrap();

        let outcome = core
            .delete_task(parent.id, DeleteMode::PromoteChildren)
            .unwrap();
        let returned_parent = outcome.deleted.iter().find(|t| t.id == parent.id).unwrap();

        let tree = core
            .get_tree(TreeFilter {
                include_deleted: true,
                ..TreeFilter::default()
            })
            .unwrap();
        let refetched_parent = tree.iter().find(|t| t.id == parent.id).unwrap();

        assert!((returned_parent.progress - refetched_parent.progress).abs() < f32::EPSILON);
    }

    #[test]
    fn delete_task_subtree_should_not_overflow_the_stack_on_a_deep_chain() {
        // Regression test: `create_task` places no limit on nesting depth,
        // so `tombstone_subtree` must walk an arbitrarily deep chain via an
        // explicit work stack rather than Rust call recursion — a naive
        // recursive walk would overflow the process stack well before this
        // many levels.
        let mut core = new_core();
        let root = core.create_task(minimal_new_task("Root")).unwrap();
        let mut current = root.id;
        for i in 0..5000 {
            let task = core
                .create_task(NewTask {
                    parent_id: Some(current),
                    ..minimal_new_task(&format!("Level {i}"))
                })
                .unwrap();
            current = task.id;
        }

        let outcome = core.delete_task(root.id, DeleteMode::Subtree).unwrap();

        assert_eq!(outcome.deleted.len(), 5001);
        assert!(outcome.deleted.iter().all(|t| t.deleted_at.is_some()));
        assert_eq!(outcome.updated, []);
    }

    #[test]
    fn delete_task_promote_children_should_move_children_to_the_deleted_tasks_parent() {
        let mut core = new_core();
        let grandparent = titled_under(&mut core, "Grandparent", None).id;
        let parent = titled_under(&mut core, "Parent", Some(grandparent)).id;
        let uncle = titled_under(&mut core, "Uncle", Some(grandparent)).id;
        let child_1 = titled_under(&mut core, "Child 1", Some(parent)).id;
        let child_2 = titled_under(&mut core, "Child 2", Some(parent)).id;
        let grandchild = titled_under(&mut core, "Grandchild", Some(child_1)).id;

        core.delete_task(parent, DeleteMode::PromoteChildren)
            .unwrap();

        for child in [child_1, child_2] {
            let stored = core.get_task(child).unwrap().unwrap();
            assert_eq!(stored.parent_id, Some(grandparent));
            assert!(stored.deleted_at.is_none());
        }
        // The children land after the grandparent's existing children, in
        // the order they had, and keep their own children.
        let order = core.sibling_order().unwrap();
        assert_eq!(
            order.children_of(Some(grandparent)),
            [uncle, child_1, child_2]
        );
        assert_eq!(order.children_of(Some(child_1)), [grandchild]);
        let stored_grandchild = core.get_task(grandchild).unwrap().unwrap();
        assert_eq!(stored_grandchild.parent_id, Some(child_1));
    }

    #[test]
    fn delete_task_promote_children_should_make_children_top_level_when_it_had_no_parent() {
        let mut core = new_core();
        let parent = titled_under(&mut core, "Parent", None).id;
        let other = titled_under(&mut core, "Other", None).id;
        let child_1 = titled_under(&mut core, "Child 1", Some(parent)).id;
        let child_2 = titled_under(&mut core, "Child 2", Some(parent)).id;

        core.delete_task(parent, DeleteMode::PromoteChildren)
            .unwrap();

        for child in [child_1, child_2] {
            let stored = core.get_task(child).unwrap().unwrap();
            assert_eq!(stored.parent_id, None);
            assert!(stored.deleted_at.is_none());
        }
        // The children join the top level after its existing tasks, in the
        // order they had.
        let order = core.sibling_order().unwrap();
        assert_eq!(order.children_of(None), [other, child_1, child_2]);
    }

    #[test]
    fn deleted_task_should_be_excluded_from_get_tree_by_default() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Task")).unwrap();

        core.delete_task(task.id, DeleteMode::Subtree).unwrap();

        let tree = core.get_tree(TreeFilter::default()).unwrap();
        assert!(!tree.iter().any(|t| t.id == task.id));
    }

    #[test]
    fn get_tree_should_include_deleted_task_when_filter_requests_it() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Task")).unwrap();

        core.delete_task(task.id, DeleteMode::Subtree).unwrap();

        let tree = core
            .get_tree(TreeFilter {
                include_deleted: true,
                ..TreeFilter::default()
            })
            .unwrap();
        assert!(tree.iter().any(|t| t.id == task.id));
    }

    #[test]
    fn restore_task_should_reject_when_task_does_not_exist_at_all() {
        let mut core = new_core();
        let missing = TaskId::new();

        let result = core.restore_task(missing);

        assert!(matches!(result, Err(CoreError::NotFound(id)) if id == missing));
    }

    #[test]
    fn set_parent_and_promote_delete_should_take_the_current_parent_from_the_fetched_task() {
        let store = CountingStore::new();
        let log = store.log();
        let mut core = Core::new(store).unwrap();
        let parent = core.create_task(minimal_new_task("Parent")).unwrap();
        let task = core
            .create_task(NewTask {
                parent_id: Some(parent.id),
                ..minimal_new_task("Task")
            })
            .unwrap();
        let parent_lookups = |from: usize| {
            log.borrow()[from..]
                .iter()
                .filter(|&&(_, method)| method == "get_parent_edge")
                .count()
        };

        // The task read at the start of each call already carries its
        // parent, so neither asks the store for that edge again.
        let start = log.borrow().len();
        core.set_parent(task.id, Some(parent.id)).unwrap();
        assert_eq!(parent_lookups(start), 0);

        let start = log.borrow().len();
        core.delete_task(task.id, DeleteMode::PromoteChildren)
            .unwrap();
        assert_eq!(parent_lookups(start), 0);
    }

    #[test]
    fn restore_task_should_report_the_parent_edge_of_a_descendant_deleted_with_its_subtree() {
        let mut core = new_core();
        let top = titled_under(&mut core, "Top", None);
        let root = titled_under(&mut core, "Root", None);
        let child = titled_under(&mut core, "Child", Some(root.id));
        core.delete_task(root.id, DeleteMode::Subtree).unwrap();

        let restored = core.restore_task(child.id).unwrap();

        // A subtree delete leaves each descendant's edge at the top level,
        // so that is where the restored task is, and where the moves that
        // read its parent act.
        let edge = core
            .store
            .transaction(|tx| tx.get_parent_edge(child.id))
            .unwrap();
        assert_eq!((restored.parent_id, edge), (None, None));
        assert!(!core.outdent_task(child.id).unwrap());
        assert!(core.move_sibling(child.id, Direction::Up).unwrap());
        assert_eq!(
            core.sibling_order().unwrap().children_of(None),
            [child.id, top.id]
        );
    }

    #[test]
    fn restore_task_should_clear_deleted_at_and_make_task_visible_again() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Task")).unwrap();
        core.delete_task(task.id, DeleteMode::Subtree).unwrap();

        let restored = core.restore_task(task.id).unwrap();

        assert!(restored.deleted_at.is_none());
        let tree = core.get_tree(TreeFilter::default()).unwrap();
        assert!(tree.iter().any(|t| t.id == task.id));
    }

    #[test]
    fn delete_task_should_keep_dependency_edges() {
        let mut core = new_core();
        let a = core.create_task(minimal_new_task("A")).unwrap();
        let b = core.create_task(minimal_new_task("B")).unwrap();
        core.add_dependency(b.id, a.id, DependencyType::FinishToStart)
            .unwrap();

        core.delete_task(a.id, DeleteMode::Subtree).unwrap();

        let stored = core.get_task(b.id).unwrap().unwrap();
        assert_eq!(
            stored.depends_on,
            vec![Dependency {
                predecessor_id: a.id,
                dep_type: DependencyType::FinishToStart,
            }]
        );
    }

    #[test]
    fn restore_task_should_bring_back_its_dependencies() {
        let mut core = new_core();
        let a = core.create_task(minimal_new_task("A")).unwrap();
        let b = core.create_task(minimal_new_task("B")).unwrap();
        core.add_dependency(b.id, a.id, DependencyType::FinishToStart)
            .unwrap();
        core.delete_task(b.id, DeleteMode::Subtree).unwrap();

        let restored = core.restore_task(b.id).unwrap();

        let expected = vec![Dependency {
            predecessor_id: a.id,
            dep_type: DependencyType::FinishToStart,
        }];
        assert_eq!(restored.depends_on, expected);
        let stored = core.get_task(b.id).unwrap().unwrap();
        assert_eq!(stored.depends_on, expected);
    }

    #[test]
    fn restore_task_should_be_idempotent_when_task_is_already_live() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Task")).unwrap();

        let restored = core.restore_task(task.id).unwrap();

        assert!(restored.deleted_at.is_none());
        assert_eq!(restored.id, task.id);
    }

    #[test]
    fn restore_task_should_bump_updated_at() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Task")).unwrap();
        core.delete_task(task.id, DeleteMode::Subtree).unwrap();

        let restored = core.restore_task(task.id).unwrap();

        assert!(restored.updated_at >= task.updated_at);
    }

    #[test]
    fn reopen_task_should_reject_when_task_not_found() {
        let mut core = new_core();
        let missing = TaskId::new();

        let result = core.reopen_task(missing);

        assert!(matches!(result, Err(CoreError::NotFound(id)) if id == missing));
    }

    #[test]
    fn reopen_task_should_clear_completed_at_and_set_status_incomplete() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Task")).unwrap();
        core.complete_task(task.id, false).unwrap();

        let reopened = core.reopen_task(task.id).unwrap();

        assert_eq!(reopened.status, TaskStatus::Incomplete);
        assert!(reopened.completed_at.is_none());
    }

    #[test]
    fn reopen_task_should_be_idempotent_when_task_already_incomplete() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Task")).unwrap();

        let reopened = core.reopen_task(task.id).unwrap();

        assert_eq!(reopened.status, TaskStatus::Incomplete);
        assert!(reopened.completed_at.is_none());
        assert_eq!(reopened.id, task.id);
    }

    #[test]
    fn reopen_task_should_bump_updated_at() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Task")).unwrap();
        let completed = core.complete_task(task.id, false).unwrap();
        let completed_updated_at = completed[0].updated_at;

        let reopened = core.reopen_task(task.id).unwrap();

        assert!(reopened.updated_at >= completed_updated_at);
    }

    #[test]
    fn delete_task_subtree_should_not_report_or_re_tombstone_an_already_deleted_child() {
        // A task tombstoned by an earlier call keeps its own parent edge,
        // so it's still listed among its parent's child edges. Deleting
        // that parent later must not report it as deleted by this call, nor
        // overwrite its original `deleted_at`.
        let mut core = new_core();
        let parent = core.create_task(minimal_new_task("Parent")).unwrap();
        let child = core
            .create_task(NewTask {
                parent_id: Some(parent.id),
                ..minimal_new_task("Child")
            })
            .unwrap();
        let first = core.delete_task(child.id, DeleteMode::Subtree).unwrap();
        let child_deleted_at = first.deleted[0].deleted_at;

        let outcome = core.delete_task(parent.id, DeleteMode::Subtree).unwrap();

        let deleted_ids: Vec<_> = outcome.deleted.iter().map(|t| t.id).collect();
        assert_eq!(deleted_ids, vec![parent.id]);
        assert_eq!(outcome.updated, []);
        let tree = core
            .get_tree(TreeFilter {
                include_deleted: true,
                ..TreeFilter::default()
            })
            .unwrap();
        let child_after = tree.iter().find(|t| t.id == child.id).unwrap();
        assert_eq!(child_after.deleted_at, child_deleted_at);
    }

    #[test]
    fn delete_task_promote_children_should_report_children_as_updated_and_task_as_deleted() {
        let mut core = new_core();
        let grandparent = core.create_task(minimal_new_task("Grandparent")).unwrap();
        let parent = core
            .create_task(NewTask {
                parent_id: Some(grandparent.id),
                ..minimal_new_task("Parent")
            })
            .unwrap();
        let child_1 = core
            .create_task(NewTask {
                parent_id: Some(parent.id),
                ..minimal_new_task("Child 1")
            })
            .unwrap();
        let child_2 = core
            .create_task(NewTask {
                parent_id: Some(parent.id),
                ..minimal_new_task("Child 2")
            })
            .unwrap();

        let outcome = core
            .delete_task(parent.id, DeleteMode::PromoteChildren)
            .unwrap();

        let deleted_ids: Vec<_> = outcome.deleted.iter().map(|t| t.id).collect();
        assert_eq!(deleted_ids, vec![parent.id]);
        assert!(outcome.deleted[0].deleted_at.is_some());
        let updated_ids: HashSet<_> = outcome.updated.iter().map(|t| t.id).collect();
        assert_eq!(updated_ids, HashSet::from([child_1.id, child_2.id]));
        assert!(outcome.updated.iter().all(|t| t.deleted_at.is_none()));
        assert!(
            outcome
                .updated
                .iter()
                .all(|t| t.parent_id == Some(grandparent.id))
        );
        assert!(!updated_ids.contains(&grandparent.id));
    }

    #[test]
    fn complete_task_should_reject_when_task_not_found() {
        let mut core = new_core();
        let missing = TaskId::new();

        let result = core.complete_task(missing, false);

        assert!(matches!(result, Err(CoreError::NotFound(id)) if id == missing));
    }

    #[test]
    fn complete_task_should_mark_leaf_task_complete_and_set_completed_at() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Leaf")).unwrap();

        let touched = core.complete_task(task.id, false).unwrap();

        assert_eq!(touched.len(), 1);
        assert_eq!(touched[0].id, task.id);
        assert_eq!(touched[0].status, TaskStatus::Complete);
        assert!(touched[0].completed_at.is_some());
    }

    #[test]
    fn complete_task_should_set_progress_one_for_completed_leaf_task() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Leaf")).unwrap();

        let touched = core.complete_task(task.id, false).unwrap();

        assert_eq!(touched.len(), 1);
        assert!((touched[0].progress - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn complete_task_should_block_when_children_incomplete_and_cascade_false() {
        let mut core = new_core();
        let parent = core.create_task(minimal_new_task("Parent")).unwrap();
        let child = core
            .create_task(NewTask {
                parent_id: Some(parent.id),
                ..minimal_new_task("Child")
            })
            .unwrap();

        let result = core.complete_task(parent.id, false);

        assert!(matches!(
            result,
            Err(CoreError::IncompleteChildren { task, incomplete })
                if task == parent.id && incomplete == vec![child.id]
        ));

        // Nothing should have been written.
        let tree = core.get_tree(TreeFilter::default()).unwrap();
        let parent_after = tree.iter().find(|t| t.id == parent.id).unwrap();
        assert_eq!(parent_after.status, TaskStatus::Incomplete);
    }

    #[test]
    fn complete_task_should_report_every_incomplete_descendant_across_multiple_levels() {
        let mut core = new_core();
        let root = core.create_task(minimal_new_task("Root")).unwrap();
        let mid = core
            .create_task(NewTask {
                parent_id: Some(root.id),
                ..minimal_new_task("Mid")
            })
            .unwrap();
        let leaf = core
            .create_task(NewTask {
                parent_id: Some(mid.id),
                ..minimal_new_task("Leaf")
            })
            .unwrap();

        let result = core.complete_task(root.id, false);

        let Err(CoreError::IncompleteChildren { task, incomplete }) = result else {
            panic!("expected IncompleteChildren, got {result:?}");
        };
        assert_eq!(task, root.id);
        let incomplete: std::collections::HashSet<_> = incomplete.into_iter().collect();
        assert_eq!(
            incomplete,
            std::collections::HashSet::from([mid.id, leaf.id]),
            "the reported set should match every task cascade:true would actually touch, \
             not just direct children"
        );

        // Nothing should have been written.
        let tree = core.get_tree(TreeFilter::default()).unwrap();
        assert!(tree.iter().all(|t| t.status == TaskStatus::Incomplete));
    }

    #[test]
    fn complete_task_should_cascade_complete_whole_subtree_when_cascade_true() {
        let mut core = new_core();
        let root = core.create_task(minimal_new_task("Root")).unwrap();
        let mid = core
            .create_task(NewTask {
                parent_id: Some(root.id),
                ..minimal_new_task("Mid")
            })
            .unwrap();
        let leaf = core
            .create_task(NewTask {
                parent_id: Some(mid.id),
                ..minimal_new_task("Leaf")
            })
            .unwrap();

        let touched = core.complete_task(root.id, true).unwrap();

        let touched_ids: std::collections::HashSet<_> = touched.iter().map(|t| t.id).collect();
        assert!(touched_ids.contains(&root.id));
        assert!(touched_ids.contains(&mid.id));
        assert!(touched_ids.contains(&leaf.id));
        assert!(touched.iter().all(|t| t.status == TaskStatus::Complete));
        assert!(touched.iter().all(|t| t.completed_at.is_some()));
    }

    #[test]
    fn complete_task_should_not_require_cascade_when_children_already_complete() {
        let mut core = new_core();
        let parent = core.create_task(minimal_new_task("Parent")).unwrap();
        let child = core
            .create_task(NewTask {
                parent_id: Some(parent.id),
                ..minimal_new_task("Child")
            })
            .unwrap();
        core.complete_task(child.id, false).unwrap();

        let touched = core.complete_task(parent.id, false).unwrap();

        assert_eq!(touched.len(), 1);
        assert_eq!(touched[0].id, parent.id);
        assert_eq!(touched[0].status, TaskStatus::Complete);
    }

    #[test]
    fn complete_task_should_return_every_task_actually_completed() {
        let mut core = new_core();
        let root = core.create_task(minimal_new_task("Root")).unwrap();
        let child_a = core
            .create_task(NewTask {
                parent_id: Some(root.id),
                ..minimal_new_task("Child A")
            })
            .unwrap();
        let child_b = core
            .create_task(NewTask {
                parent_id: Some(root.id),
                ..minimal_new_task("Child B")
            })
            .unwrap();
        // Pre-complete child_b so cascading over it doesn't double-count it.
        core.complete_task(child_b.id, false).unwrap();

        let touched = core.complete_task(root.id, true).unwrap();

        let touched_ids: std::collections::HashSet<_> = touched.iter().map(|t| t.id).collect();
        assert!(touched_ids.contains(&root.id));
        assert!(touched_ids.contains(&child_a.id));
        // child_b was already complete before this call, so it is not
        // counted as "actually completed" by this call.
        assert!(!touched_ids.contains(&child_b.id));
        assert_eq!(touched.len(), 2);
    }

    #[test]
    fn complete_task_should_bump_updated_at() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Task")).unwrap();

        let touched = core.complete_task(task.id, false).unwrap();

        assert_eq!(touched[0].created_at, task.created_at);
        assert!(touched[0].updated_at >= task.updated_at);
    }

    #[test]
    fn complete_task_should_be_a_no_op_when_already_complete() {
        // Regression test: completing an already-complete leaf task must not
        // return it as newly touched or clobber its original `completed_at`,
        // matching `mark_complete_subtree`'s own "skip already-complete"
        // behavior under cascade.
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Task")).unwrap();
        let first = core.complete_task(task.id, false).unwrap();
        let first_completed_at = first[0].completed_at;

        let second = core.complete_task(task.id, false).unwrap();

        assert_eq!(second, []);
        let stored = core
            .get_tree(TreeFilter::default())
            .unwrap()
            .into_iter()
            .find(|t| t.id == task.id)
            .unwrap();
        assert_eq!(stored.completed_at, first_completed_at);
    }

    #[test]
    fn complete_task_subtree_should_not_overflow_stack_on_a_deep_chain() {
        // Regression test mirroring
        // `delete_task_subtree_should_not_overflow_the_stack_on_a_deep_chain`:
        // the subtree walk must use an explicit work stack, not Rust call
        // recursion, so it doesn't overflow on a deep chain.
        let mut core = new_core();
        let root = core.create_task(minimal_new_task("Root")).unwrap();
        let mut current = root.id;
        for i in 0..5000 {
            let task = core
                .create_task(NewTask {
                    parent_id: Some(current),
                    ..minimal_new_task(&format!("Level {i}"))
                })
                .unwrap();
            current = task.id;
        }

        let touched = core.complete_task(root.id, true).unwrap();

        assert_eq!(touched.len(), 5001);
        assert!(touched.iter().all(|t| t.status == TaskStatus::Complete));
    }

    #[test]
    fn get_task_should_return_none_when_task_not_found() {
        let core = new_core();
        let missing = TaskId::new();

        let result = core.get_task(missing);

        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn get_task_should_return_the_task_when_it_exists() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Task")).unwrap();

        let result = core.get_task(task.id).unwrap();

        assert_eq!(result, Some(task));
    }

    #[test]
    fn get_task_should_return_none_for_soft_deleted_task() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Task")).unwrap();
        core.delete_task(task.id, DeleteMode::Subtree).unwrap();

        let result = core.get_task(task.id);

        assert!(matches!(result, Ok(None)));
    }

    fn titled_under(core: &mut Core<InMemoryStore>, title: &str, parent: Option<TaskId>) -> Task {
        core.create_task(NewTask {
            parent_id: parent,
            ..minimal_new_task(title)
        })
        .unwrap()
    }

    #[test]
    fn list_children_should_return_creation_order_by_default() {
        let mut core = new_core();
        let p = titled_under(&mut core, "P", None);
        for title in ["a", "b", "c"] {
            titled_under(&mut core, title, Some(p.id));
        }

        let titles: Vec<_> = core
            .list_children(p.id)
            .unwrap()
            .into_iter()
            .map(|t| t.title)
            .collect();

        assert_eq!(titles, ["a", "b", "c"]);
    }

    #[test]
    fn get_tree_should_respect_sibling_order() {
        let mut core = new_core();
        let p = titled_under(&mut core, "P", None);
        let a = titled_under(&mut core, "a", Some(p.id));
        let b = titled_under(&mut core, "b", Some(p.id));
        let q = titled_under(&mut core, "Q", None);

        let ids: Vec<_> = core
            .get_tree(TreeFilter::default())
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect();

        assert_eq!(ids, [p.id, a.id, b.id, q.id]);
    }

    #[test]
    fn sibling_order_should_key_top_level_under_none() {
        let mut core = new_core();
        let r1 = titled_under(&mut core, "r1", None);
        let r2 = titled_under(&mut core, "r2", None);
        let child = titled_under(&mut core, "c", Some(r1.id));

        let order = core.sibling_order().unwrap();

        assert_eq!(order.children_of(None), [r1.id, r2.id]);
        assert_eq!(order.children_of(Some(r1.id)), [child.id]);
        assert_eq!(order.children_of(Some(r2.id)), []);
    }

    #[test]
    fn sibling_order_should_skip_deleted_tasks() {
        let mut core = new_core();
        let r1 = titled_under(&mut core, "r1", None);
        let r2 = titled_under(&mut core, "r2", None);
        core.delete_task(r1.id, DeleteMode::Subtree).unwrap();

        let order = core.sibling_order().unwrap();

        assert_eq!(order.children_of(None), [r2.id]);
    }

    /// Creates `n` top-level-or-under-`parent` tasks titled a, b, c, ...
    fn siblings(core: &mut Core<InMemoryStore>, parent: Option<TaskId>, n: usize) -> Vec<TaskId> {
        (0..n)
            .map(|i| titled_under(core, &format!("t{i}"), parent).id)
            .collect()
    }

    #[test]
    fn move_sibling_should_swap_with_next_sibling() {
        let mut core = new_core();
        let p = core.create_task(minimal_new_task("p")).unwrap().id;
        let s = siblings(&mut core, Some(p), 3);

        let moved = core.move_sibling(s[0], Direction::Down).unwrap();

        assert!(moved);
        let order = core.sibling_order().unwrap();
        assert_eq!(order.children_of(Some(p)), [s[1], s[0], s[2]]);
    }

    #[test]
    fn move_sibling_should_swap_with_previous_sibling() {
        let mut core = new_core();
        let p = core.create_task(minimal_new_task("p")).unwrap().id;
        let s = siblings(&mut core, Some(p), 3);

        let moved = core.move_sibling(s[2], Direction::Up).unwrap();

        assert!(moved);
        let order = core.sibling_order().unwrap();
        assert_eq!(order.children_of(Some(p)), [s[0], s[2], s[1]]);
    }

    #[test]
    fn move_sibling_should_skip_deleted_siblings() {
        let mut core = new_core();
        let p = core.create_task(minimal_new_task("p")).unwrap().id;
        let s = siblings(&mut core, Some(p), 3);
        core.delete_task(s[1], DeleteMode::PromoteChildren).unwrap();

        core.move_sibling(s[0], Direction::Down).unwrap();

        let order = core.sibling_order().unwrap();
        assert_eq!(order.children_of(Some(p)), [s[2], s[0]]);
    }

    #[test]
    fn move_sibling_should_be_noop_at_ends() {
        let mut core = new_core();
        let p = core.create_task(minimal_new_task("p")).unwrap().id;
        let s = siblings(&mut core, Some(p), 2);
        let before = core.get_task(s[0]).unwrap().unwrap();

        let up = core.move_sibling(s[0], Direction::Up).unwrap();
        let down = core.move_sibling(s[1], Direction::Down).unwrap();

        assert!(!up && !down);
        let order = core.sibling_order().unwrap();
        assert_eq!(order.children_of(Some(p)), [s[0], s[1]]);
        assert_eq!(core.get_task(s[0]).unwrap().unwrap(), before);
    }

    #[test]
    fn move_sibling_should_reorder_top_level_for_a_top_level_task() {
        let mut core = new_core();
        let s = siblings(&mut core, None, 3);

        core.move_sibling(s[2], Direction::Up).unwrap();

        let order = core.sibling_order().unwrap();
        assert_eq!(order.children_of(None), [s[0], s[2], s[1]]);
    }

    #[test]
    fn move_sibling_should_reject_unknown_task() {
        let mut core = new_core();
        let missing = TaskId::new();

        let result = core.move_sibling(missing, Direction::Up);

        assert!(matches!(result, Err(CoreError::NotFound(id)) if id == missing));
    }

    #[test]
    fn indent_task_should_become_last_child_of_previous_sibling() {
        let mut core = new_core();
        let s = siblings(&mut core, None, 3);
        let existing = titled_under(&mut core, "kid", Some(s[0])).id;

        let changed = core.indent_task(s[1]).unwrap();

        assert!(changed);
        let order = core.sibling_order().unwrap();
        assert_eq!(order.children_of(None), [s[0], s[2]]);
        assert_eq!(order.children_of(Some(s[0])), [existing, s[1]]);
        assert_eq!(core.get_task(s[1]).unwrap().unwrap().parent_id, Some(s[0]));
    }

    #[test]
    fn indent_task_should_be_noop_when_first_sibling() {
        let mut core = new_core();
        let s = siblings(&mut core, None, 2);
        let before = core.get_task(s[0]).unwrap().unwrap();

        let changed = core.indent_task(s[0]).unwrap();

        assert!(!changed);
        assert_eq!(
            core.sibling_order().unwrap().children_of(None),
            [s[0], s[1]]
        );
        assert_eq!(core.get_task(s[0]).unwrap().unwrap(), before);
    }

    #[test]
    fn indent_task_should_skip_deleted_previous_sibling() {
        let mut core = new_core();
        let p = core.create_task(minimal_new_task("p")).unwrap().id;
        let s = siblings(&mut core, Some(p), 3);
        core.delete_task(s[1], DeleteMode::PromoteChildren).unwrap();

        core.indent_task(s[2]).unwrap();

        let order = core.sibling_order().unwrap();
        assert_eq!(order.children_of(Some(s[0])), [s[2]]);
    }

    #[test]
    fn indent_task_should_keep_subtree_intact() {
        let mut core = new_core();
        let s = siblings(&mut core, None, 2);
        let kid = titled_under(&mut core, "kid", Some(s[1])).id;
        let grandkid = titled_under(&mut core, "gk", Some(kid)).id;

        core.indent_task(s[1]).unwrap();

        let order = core.sibling_order().unwrap();
        assert_eq!(order.children_of(Some(s[1])), [kid]);
        assert_eq!(order.children_of(Some(kid)), [grandkid]);
    }

    #[test]
    fn indent_task_should_reject_unknown_task() {
        let mut core = new_core();
        let missing = TaskId::new();

        let result = core.indent_task(missing);

        assert!(matches!(result, Err(CoreError::NotFound(id)) if id == missing));
    }

    #[test]
    fn outdent_task_should_land_right_after_its_old_parent() {
        let mut core = new_core();
        let g = core.create_task(minimal_new_task("g")).unwrap().id;
        let p = titled_under(&mut core, "p", Some(g)).id;
        let after = titled_under(&mut core, "after", Some(g)).id;
        let x = titled_under(&mut core, "x", Some(p)).id;

        let changed = core.outdent_task(x).unwrap();

        assert!(changed);
        let order = core.sibling_order().unwrap();
        assert_eq!(order.children_of(Some(g)), [p, x, after]);
        assert_eq!(order.children_of(Some(p)), []);
        assert_eq!(core.get_task(x).unwrap().unwrap().parent_id, Some(g));
    }

    #[test]
    fn outdent_task_should_promote_to_top_level_when_parent_is_top_level() {
        let mut core = new_core();
        let p = core.create_task(minimal_new_task("p")).unwrap().id;
        let next = core.create_task(minimal_new_task("next")).unwrap().id;
        let x = titled_under(&mut core, "x", Some(p)).id;

        let changed = core.outdent_task(x).unwrap();

        assert!(changed);
        let order = core.sibling_order().unwrap();
        assert_eq!(order.children_of(None), [p, x, next]);
        assert_eq!(core.get_task(x).unwrap().unwrap().parent_id, None);
    }

    #[test]
    fn outdent_task_should_be_a_no_op_at_top_level() {
        let mut core = new_core();
        let s = siblings(&mut core, None, 2);
        let before = core.get_task(s[1]).unwrap().unwrap();

        let changed = core.outdent_task(s[1]).unwrap();

        assert!(!changed);
        assert_eq!(
            core.sibling_order().unwrap().children_of(None),
            [s[0], s[1]]
        );
        assert_eq!(core.get_task(s[1]).unwrap().unwrap(), before);
    }

    #[test]
    fn outdent_task_should_reject_deleted_task() {
        let mut core = new_core();
        let p = core.create_task(minimal_new_task("p")).unwrap().id;
        let x = titled_under(&mut core, "x", Some(p)).id;
        core.delete_task(x, DeleteMode::Subtree).unwrap();

        let result = core.outdent_task(x);

        assert!(matches!(result, Err(CoreError::NotFound(id)) if id == x));
    }

    #[test]
    fn list_children_should_return_empty_vec_when_task_has_no_children() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Leaf")).unwrap();

        let children = core.list_children(task.id).unwrap();

        assert_eq!(children, []);
    }

    #[test]
    fn list_children_should_return_only_direct_children_not_grandchildren() {
        let mut core = new_core();
        let root = core.create_task(minimal_new_task("Root")).unwrap();
        let mid = core
            .create_task(NewTask {
                parent_id: Some(root.id),
                ..minimal_new_task("Mid")
            })
            .unwrap();
        core.create_task(NewTask {
            parent_id: Some(mid.id),
            ..minimal_new_task("Leaf")
        })
        .unwrap();

        let children = core.list_children(root.id).unwrap();

        assert_eq!(children, vec![mid]);
    }

    #[test]
    fn list_children_should_return_empty_vec_for_unknown_task_id() {
        let core = new_core();
        let missing = TaskId::new();

        let children = core.list_children(missing).unwrap();

        assert_eq!(children, []);
    }

    #[test]
    fn set_parent_should_reject_when_task_not_found() {
        let mut core = new_core();
        let missing = TaskId::new();
        let parent = core.create_task(minimal_new_task("Parent")).unwrap();

        let result = core.set_parent(missing, Some(parent.id));

        assert!(matches!(result, Err(CoreError::NotFound(id)) if id == missing));
    }

    #[test]
    fn set_parent_should_reject_missing_parent() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Task")).unwrap();
        let missing_parent = TaskId::new();

        let deleted_parent = core.create_task(minimal_new_task("Deleted")).unwrap();
        core.delete_task(deleted_parent.id, DeleteMode::Subtree)
            .unwrap();
        let before = store_snapshot(&core);

        let missing = core.set_parent(task.id, Some(missing_parent));
        let deleted = core.set_parent(task.id, Some(deleted_parent.id));

        assert!(matches!(missing, Err(CoreError::NotFound(id)) if id == missing_parent));
        assert!(matches!(deleted, Err(CoreError::NotFound(id)) if id == deleted_parent.id));
        assert_eq!(store_snapshot(&core), before);
    }

    #[test]
    fn set_parent_should_move_task_under_new_parent() {
        let mut core = new_core();
        let old_parent = titled_under(&mut core, "Old parent", None);
        let new_parent = titled_under(&mut core, "New parent", None);
        let existing = titled_under(&mut core, "Existing", Some(new_parent.id));
        let task = titled_under(&mut core, "Task", Some(old_parent.id));

        let updated = core.set_parent(task.id, Some(new_parent.id)).unwrap();

        assert_eq!(updated.parent_id, Some(new_parent.id));
        let stored = core.get_task(task.id).unwrap().unwrap();
        assert_eq!(stored.parent_id, Some(new_parent.id));
        // The task leaves its old parent and lands after the new parent's
        // existing children.
        let order = core.sibling_order().unwrap();
        assert_eq!(order.children_of(Some(old_parent.id)), []);
        assert_eq!(
            order.children_of(Some(new_parent.id)),
            [existing.id, task.id]
        );
    }

    #[test]
    fn set_parent_should_promote_to_top_level_when_none() {
        let mut core = new_core();
        let parent = titled_under(&mut core, "Parent", None);
        let task = titled_under(&mut core, "Task", Some(parent.id));
        let other_root = titled_under(&mut core, "Other root", None);

        let updated = core.set_parent(task.id, None).unwrap();

        assert_eq!(updated.parent_id, None);
        let stored = core.get_task(task.id).unwrap().unwrap();
        assert_eq!(stored.parent_id, None);
        // The task leaves its parent and lands after the existing roots.
        let order = core.sibling_order().unwrap();
        assert_eq!(order.children_of(Some(parent.id)), []);
        assert_eq!(order.children_of(None), [parent.id, other_root.id, task.id]);
    }

    #[test]
    fn set_parent_should_allow_parent_and_child_of_different_task_types() {
        let mut core = new_core();
        let goal = TaskType {
            key: "goal".to_owned(),
            label: "Goal".to_owned(),
            color: None,
            sort_order: 1,
        };
        core.upsert_task_type(goal).unwrap();
        let parent = core
            .create_task(NewTask {
                type_key: Some("goal".to_owned()),
                ..minimal_new_task("Parent")
            })
            .unwrap();
        let child = core.create_task(minimal_new_task("Child")).unwrap();
        assert_ne!(parent.type_key, child.type_key);

        let result = core.set_parent(child.id, Some(parent.id));

        assert!(result.is_ok());
        assert_eq!(result.unwrap().parent_id, Some(parent.id));
    }

    #[test]
    fn set_parent_should_reject_self_parenting_with_circular_hierarchy_error() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Task")).unwrap();

        let result = core.set_parent(task.id, Some(task.id));

        assert!(matches!(
            result,
            Err(CoreError::CircularHierarchy { task: t, attempted_parent })
                if t == task.id && attempted_parent == task.id
        ));
    }

    #[test]
    fn set_parent_should_reject_making_a_task_its_own_ancestor() {
        let mut core = new_core();
        let root = titled_under(&mut core, "Root", None);
        let task = titled_under(&mut core, "Task", Some(root.id));
        let child = titled_under(&mut core, "Child", Some(task.id));
        let grandchild = titled_under(&mut core, "Grandchild", Some(child.id));
        let before = store_snapshot(&core);

        let under_child = core.set_parent(task.id, Some(child.id));
        let under_grandchild = core.set_parent(task.id, Some(grandchild.id));

        assert!(matches!(
            under_child,
            Err(CoreError::CircularHierarchy { task: t, attempted_parent })
                if t == task.id && attempted_parent == child.id
        ));
        assert!(matches!(
            under_grandchild,
            Err(CoreError::CircularHierarchy { task: t, attempted_parent })
                if t == task.id && attempted_parent == grandchild.id
        ));
        // A rejected move changes nothing: no row and no edge.
        assert_eq!(store_snapshot(&core), before);
    }

    #[test]
    fn set_parent_should_change_nothing_when_parent_is_unchanged() {
        let mut core = new_core();
        let parent = titled_under(&mut core, "Parent", None);
        let task = titled_under(&mut core, "Task", Some(parent.id));
        titled_under(&mut core, "Later sibling", Some(parent.id));
        let root = titled_under(&mut core, "Root", None);
        titled_under(&mut core, "Later root", None);
        let before = store_snapshot(&core);

        let under_same_parent = core.set_parent(task.id, Some(parent.id)).unwrap();
        let still_top_level = core.set_parent(root.id, None).unwrap();

        // Neither `updated_at` nor the position among the siblings moves.
        assert_eq!(under_same_parent, task);
        assert_eq!(still_top_level, root);
        assert_eq!(store_snapshot(&core), before);
    }

    #[test]
    fn set_parent_should_bump_updated_at() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Task")).unwrap();
        let new_parent = core.create_task(minimal_new_task("New parent")).unwrap();

        let updated = core.set_parent(task.id, Some(new_parent.id)).unwrap();

        assert_eq!(updated.created_at, task.created_at);
        assert!(updated.updated_at >= task.updated_at);
    }

    #[test]
    fn get_task_should_return_current_progress_reflecting_direct_children() {
        let mut core = new_core();
        let parent = core.create_task(minimal_new_task("Parent")).unwrap();
        let child_a = core
            .create_task(NewTask {
                parent_id: Some(parent.id),
                ..minimal_new_task("Child A")
            })
            .unwrap();
        core.complete_task(child_a.id, false).unwrap();
        core.create_task(NewTask {
            parent_id: Some(parent.id),
            ..minimal_new_task("Child B")
        })
        .unwrap();

        let fetched = core.get_task(parent.id).unwrap().unwrap();

        assert!((fetched.progress - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn list_children_should_return_each_childs_own_computed_progress() {
        let mut core = new_core();
        let root = core.create_task(minimal_new_task("Root")).unwrap();
        let mid = core
            .create_task(NewTask {
                parent_id: Some(root.id),
                ..minimal_new_task("Mid")
            })
            .unwrap();
        let grandchild = core
            .create_task(NewTask {
                parent_id: Some(mid.id),
                ..minimal_new_task("Grandchild")
            })
            .unwrap();
        core.complete_task(grandchild.id, false).unwrap();

        let children = core.list_children(root.id).unwrap();

        assert_eq!(children.len(), 1);
        assert_eq!(children[0].id, mid.id);
        // `mid`'s own progress is a rollup of ITS direct child
        // (`grandchild`, now Complete) — not `root`'s placeholder and not
        // `grandchild`'s own progress.
        assert!((children[0].progress - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn update_task_should_return_current_progress_not_stale_placeholder() {
        let mut core = new_core();
        let parent = core.create_task(minimal_new_task("Parent")).unwrap();
        let child = core
            .create_task(NewTask {
                parent_id: Some(parent.id),
                ..minimal_new_task("Child")
            })
            .unwrap();
        core.complete_task(child.id, false).unwrap();

        let updated = core
            .update_task(
                parent.id,
                TaskPatch {
                    title: Field::Set("Parent renamed".to_owned()),
                    ..TaskPatch::default()
                },
            )
            .unwrap();

        assert_eq!(updated.len(), 1);
        assert!((updated[0].progress - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn complete_task_cascade_should_return_progress_one_for_every_completed_leaf() {
        let mut core = new_core();
        let root = core.create_task(minimal_new_task("Root")).unwrap();
        let mid = core
            .create_task(NewTask {
                parent_id: Some(root.id),
                ..minimal_new_task("Mid")
            })
            .unwrap();
        let leaf = core
            .create_task(NewTask {
                parent_id: Some(mid.id),
                ..minimal_new_task("Leaf")
            })
            .unwrap();

        let touched = core.complete_task(root.id, true).unwrap();

        for id in [root.id, mid.id, leaf.id] {
            let task = touched.iter().find(|t| t.id == id).unwrap();
            assert!(
                (task.progress - 1.0).abs() < f32::EPSILON,
                "expected progress 1.0 for {id:?}, got {}",
                task.progress
            );
        }
    }

    #[test]
    fn set_parent_should_leave_siblings_progress_unaffected_by_reparenting() {
        let mut core = new_core();
        let old_parent = core.create_task(minimal_new_task("Old parent")).unwrap();
        let new_parent = core.create_task(minimal_new_task("New parent")).unwrap();
        let task = core
            .create_task(NewTask {
                parent_id: Some(old_parent.id),
                ..minimal_new_task("Task")
            })
            .unwrap();
        let sibling = core
            .create_task(NewTask {
                parent_id: Some(old_parent.id),
                ..minimal_new_task("Sibling")
            })
            .unwrap();
        core.complete_task(sibling.id, false).unwrap();
        let sibling_progress_before = core.get_task(sibling.id).unwrap().unwrap().progress;

        core.set_parent(task.id, Some(new_parent.id)).unwrap();

        let sibling_progress_after = core.get_task(sibling.id).unwrap().unwrap().progress;
        assert!((sibling_progress_after - sibling_progress_before).abs() < f32::EPSILON);
    }

    #[test]
    fn add_dependency_should_record_predecessor_on_the_task() {
        let mut core = new_core();
        let predecessor = core.create_task(minimal_new_task("Predecessor")).unwrap();
        let task = core.create_task(minimal_new_task("Task")).unwrap();

        let updated = core
            .add_dependency(task.id, predecessor.id, DependencyType::FinishToStart)
            .unwrap();

        let expected = vec![Dependency {
            predecessor_id: predecessor.id,
            dep_type: DependencyType::FinishToStart,
        }];
        assert_eq!(updated.id, task.id);
        assert_eq!(updated.depends_on, expected);
        let stored = core.get_task(task.id).unwrap().unwrap();
        assert_eq!(stored.depends_on, expected);
        let stored_predecessor = core.get_task(predecessor.id).unwrap().unwrap();
        assert_eq!(stored_predecessor.depends_on, []);
    }

    #[test]
    fn add_dependency_should_accept_several_predecessors_for_one_task() {
        let mut core = new_core();
        let first = core.create_task(minimal_new_task("First")).unwrap();
        let second = core.create_task(minimal_new_task("Second")).unwrap();
        let task = core.create_task(minimal_new_task("Task")).unwrap();

        core.add_dependency(task.id, first.id, DependencyType::FinishToStart)
            .unwrap();
        let updated = core
            .add_dependency(task.id, second.id, DependencyType::StartToStart)
            .unwrap();

        assert_eq!(
            updated.depends_on,
            vec![
                Dependency {
                    predecessor_id: first.id,
                    dep_type: DependencyType::FinishToStart,
                },
                Dependency {
                    predecessor_id: second.id,
                    dep_type: DependencyType::StartToStart,
                },
            ]
        );
    }

    #[test]
    fn add_dependency_should_replace_type_on_existing_pair() {
        let mut core = new_core();
        let first = core.create_task(minimal_new_task("First")).unwrap();
        let second = core.create_task(minimal_new_task("Second")).unwrap();
        let task = core.create_task(minimal_new_task("Task")).unwrap();
        core.add_dependency(task.id, first.id, DependencyType::FinishToStart)
            .unwrap();
        core.add_dependency(task.id, second.id, DependencyType::FinishToStart)
            .unwrap();

        let updated = core
            .add_dependency(task.id, first.id, DependencyType::FinishToFinish)
            .unwrap();

        assert_eq!(
            updated.depends_on,
            vec![
                Dependency {
                    predecessor_id: first.id,
                    dep_type: DependencyType::FinishToFinish,
                },
                Dependency {
                    predecessor_id: second.id,
                    dep_type: DependencyType::FinishToStart,
                },
            ]
        );
    }

    #[test]
    fn add_dependency_should_reject_self_dependency() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Task")).unwrap();

        let result = core.add_dependency(task.id, task.id, DependencyType::FinishToStart);

        assert!(matches!(result, Err(CoreError::SelfDependency(id)) if id == task.id));
        let stored = core.get_task(task.id).unwrap().unwrap();
        assert_eq!(stored.depends_on, []);
        assert_eq!(stored.updated_at, task.updated_at);
    }

    #[test]
    fn add_dependency_should_reject_when_task_not_found() {
        let mut core = new_core();
        let predecessor = core.create_task(minimal_new_task("Predecessor")).unwrap();
        let missing = TaskId::new();

        let result = core.add_dependency(missing, predecessor.id, DependencyType::FinishToStart);

        assert!(matches!(result, Err(CoreError::NotFound(id)) if id == missing));
    }

    #[test]
    fn add_dependency_should_reject_when_predecessor_not_found() {
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Task")).unwrap();
        let missing = TaskId::new();

        let result = core.add_dependency(task.id, missing, DependencyType::FinishToStart);

        assert!(matches!(result, Err(CoreError::NotFound(id)) if id == missing));
        let stored = core.get_task(task.id).unwrap().unwrap();
        assert_eq!(stored.depends_on, []);
        assert_eq!(stored.updated_at, task.updated_at);
    }

    #[test]
    fn add_dependency_should_reject_soft_deleted_predecessor() {
        let mut core = new_core();
        let predecessor = core.create_task(minimal_new_task("Predecessor")).unwrap();
        let task = core.create_task(minimal_new_task("Task")).unwrap();
        core.delete_task(predecessor.id, DeleteMode::Subtree)
            .unwrap();

        let result = core.add_dependency(task.id, predecessor.id, DependencyType::FinishToStart);

        assert!(matches!(result, Err(CoreError::NotFound(id)) if id == predecessor.id));
        let stored = core.get_task(task.id).unwrap().unwrap();
        assert_eq!(stored.depends_on, []);
    }

    #[test]
    fn add_dependency_should_bump_updated_at() {
        let mut core = new_core();
        let predecessor = core.create_task(minimal_new_task("Predecessor")).unwrap();
        let task = core.create_task(minimal_new_task("Task")).unwrap();

        let updated = core
            .add_dependency(task.id, predecessor.id, DependencyType::FinishToStart)
            .unwrap();

        assert_eq!(updated.created_at, task.created_at);
        assert!(updated.updated_at >= task.updated_at);
        let stored = core.get_task(task.id).unwrap().unwrap();
        assert_eq!(stored.updated_at, updated.updated_at);
    }

    #[test]
    fn add_dependency_should_change_nothing_when_pair_already_linked_with_that_type() {
        let store = CountingStore::new();
        let log = store.log();
        let mut core = Core::new(store).unwrap();
        let task = core.create_task(minimal_new_task("Task")).unwrap();
        let predecessor = core.create_task(minimal_new_task("Predecessor")).unwrap();
        let done_child = core
            .create_task(NewTask {
                parent_id: Some(task.id),
                ..minimal_new_task("Done child")
            })
            .unwrap();
        core.create_task(NewTask {
            parent_id: Some(task.id),
            ..minimal_new_task("Open child")
        })
        .unwrap();
        core.complete_task(done_child.id, false).unwrap();
        let before = core
            .add_dependency(task.id, predecessor.id, DependencyType::StartToStart)
            .unwrap();
        let start = log.borrow().len();

        let returned = core
            .add_dependency(task.id, predecessor.id, DependencyType::StartToStart)
            .unwrap();

        let calls = log.borrow()[start..].to_vec();
        assert!(
            calls
                .iter()
                .all(|&(_, m)| m != "put_task" && m != "add_dependency_edge"),
            "calls: {calls:?}"
        );
        assert_eq!(returned, before);
        assert!((returned.progress - 0.5).abs() < f32::EPSILON);
        let stored = core.get_task(task.id).unwrap().unwrap();
        assert_eq!(stored, before);
    }

    #[test]
    fn add_dependency_should_reject_direct_parent_as_predecessor() {
        let mut core = new_core();
        let parent = titled_under(&mut core, "Parent", None);
        let task = titled_under(&mut core, "Task", Some(parent.id));

        let result = core.add_dependency(task.id, parent.id, DependencyType::FinishToStart);

        assert!(matches!(
            result,
            Err(CoreError::DependsOnRelative { task: t, other })
                if t == task.id && other == parent.id
        ));
    }

    #[test]
    fn add_dependency_should_reject_distant_ancestor_as_predecessor() {
        let mut core = new_core();
        let great_grandparent = titled_under(&mut core, "Great-grandparent", None);
        let grandparent = titled_under(&mut core, "Grandparent", Some(great_grandparent.id));
        let parent = titled_under(&mut core, "Parent", Some(grandparent.id));
        let task = titled_under(&mut core, "Task", Some(parent.id));

        let result =
            core.add_dependency(task.id, great_grandparent.id, DependencyType::FinishToStart);

        assert!(matches!(
            result,
            Err(CoreError::DependsOnRelative { task: t, other })
                if t == task.id && other == great_grandparent.id
        ));
    }

    #[test]
    fn add_dependency_should_reject_descendant_as_predecessor() {
        let mut core = new_core();
        let task = titled_under(&mut core, "Task", None);
        let child = titled_under(&mut core, "Child", Some(task.id));
        let grandchild = titled_under(&mut core, "Grandchild", Some(child.id));

        let result = core.add_dependency(task.id, grandchild.id, DependencyType::FinishToStart);

        assert!(matches!(
            result,
            Err(CoreError::DependsOnRelative { task: t, other })
                if t == task.id && other == grandchild.id
        ));
    }

    #[test]
    fn add_dependency_should_allow_sibling_as_predecessor() {
        let mut core = new_core();
        let parent = titled_under(&mut core, "Parent", None);
        let sibling = titled_under(&mut core, "Sibling", Some(parent.id));
        let task = titled_under(&mut core, "Task", Some(parent.id));

        let updated = core
            .add_dependency(task.id, sibling.id, DependencyType::FinishToStart)
            .unwrap();

        assert_eq!(
            updated.depends_on,
            vec![Dependency {
                predecessor_id: sibling.id,
                dep_type: DependencyType::FinishToStart,
            }]
        );
    }

    #[test]
    fn add_dependency_should_write_no_edge_when_rejected() {
        let mut core = new_core();
        let parent = titled_under(&mut core, "Parent", None);
        let task = titled_under(&mut core, "Task", Some(parent.id));
        let parent_before = core.get_task(parent.id).unwrap().unwrap();
        let task_before = core.get_task(task.id).unwrap().unwrap();

        // Rejected in both directions: `task` on its parent, and `parent`
        // on its child.
        let upward = core.add_dependency(task.id, parent.id, DependencyType::FinishToStart);
        let downward = core.add_dependency(parent.id, task.id, DependencyType::FinishToStart);

        assert!(matches!(upward, Err(CoreError::DependsOnRelative { .. })));
        assert!(matches!(downward, Err(CoreError::DependsOnRelative { .. })));
        let task_after = core.get_task(task.id).unwrap().unwrap();
        assert_eq!(task_after.depends_on, []);
        assert_eq!(task_after.updated_at, task_before.updated_at);
        let parent_after = core.get_task(parent.id).unwrap().unwrap();
        assert_eq!(parent_after.depends_on, []);
        assert_eq!(parent_after.updated_at, parent_before.updated_at);
    }

    #[test]
    fn add_dependency_should_reject_direct_cycle() {
        let mut core = new_core();
        let a = core.create_task(minimal_new_task("A")).unwrap();
        let b = core.create_task(minimal_new_task("B")).unwrap();
        core.add_dependency(b.id, a.id, DependencyType::FinishToStart)
            .unwrap();
        let a_before = core.get_task(a.id).unwrap().unwrap();

        let result = core.add_dependency(a.id, b.id, DependencyType::FinishToStart);

        assert!(matches!(
            result,
            Err(CoreError::CircularDependency { cycle }) if cycle == vec![a.id, b.id, a.id]
        ));
        let a_after = core.get_task(a.id).unwrap().unwrap();
        assert_eq!(a_after.depends_on, []);
        assert_eq!(a_after.updated_at, a_before.updated_at);
    }

    #[test]
    fn add_dependency_should_reject_cycle_through_transitive_predecessor() {
        let mut core = new_core();
        let a = core.create_task(minimal_new_task("A")).unwrap();
        let b = core.create_task(minimal_new_task("B")).unwrap();
        let c = core.create_task(minimal_new_task("C")).unwrap();
        core.add_dependency(b.id, a.id, DependencyType::FinishToStart)
            .unwrap();
        core.add_dependency(c.id, b.id, DependencyType::FinishToStart)
            .unwrap();

        let result = core.add_dependency(a.id, c.id, DependencyType::FinishToStart);

        assert!(matches!(
            result,
            Err(CoreError::CircularDependency { cycle }) if cycle == vec![a.id, c.id, b.id, a.id]
        ));
        let stored = core.get_task(a.id).unwrap().unwrap();
        assert_eq!(stored.depends_on, []);
    }

    #[test]
    fn add_dependency_should_report_the_cycle_path_from_task_back_to_itself() {
        // `predecessor` depends on a dead-end branch (`decoy`, which depends
        // on `other`) and on `middle`, which depends on `task`. Only the
        // `middle` branch leads back to `task`, so the reported path must
        // skip the decoy branch entirely.
        let mut core = new_core();
        let task = core.create_task(minimal_new_task("Task")).unwrap();
        let other = core.create_task(minimal_new_task("Other")).unwrap();
        let decoy = core.create_task(minimal_new_task("Decoy")).unwrap();
        let middle = core.create_task(minimal_new_task("Middle")).unwrap();
        let predecessor = core.create_task(minimal_new_task("Predecessor")).unwrap();
        for (successor, pred) in [
            (decoy.id, other.id),
            (middle.id, task.id),
            (predecessor.id, decoy.id),
            (predecessor.id, middle.id),
        ] {
            core.add_dependency(successor, pred, DependencyType::FinishToStart)
                .unwrap();
        }

        let result = core.add_dependency(task.id, predecessor.id, DependencyType::FinishToStart);

        let Err(CoreError::CircularDependency { cycle }) = result else {
            panic!("expected CircularDependency, got {result:?}");
        };
        assert_eq!(cycle, vec![task.id, predecessor.id, middle.id, task.id]);
        // Past the new edge itself, each task in the path depends on the
        // next through an edge that already exists.
        for pair in cycle[1..].windows(2) {
            let stored = core.get_task(pair[0]).unwrap().unwrap();
            assert!(
                stored
                    .depends_on
                    .iter()
                    .any(|d| d.predecessor_id == pair[1]),
                "{:?} does not depend on {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn add_dependency_should_allow_redundant_edge_that_closes_no_cycle() {
        let mut core = new_core();
        let a = core.create_task(minimal_new_task("A")).unwrap();
        let b = core.create_task(minimal_new_task("B")).unwrap();
        let c = core.create_task(minimal_new_task("C")).unwrap();
        core.add_dependency(b.id, a.id, DependencyType::FinishToStart)
            .unwrap();
        core.add_dependency(c.id, b.id, DependencyType::FinishToStart)
            .unwrap();

        let updated = core
            .add_dependency(c.id, a.id, DependencyType::FinishToStart)
            .unwrap();

        let predecessors: Vec<TaskId> = updated
            .depends_on
            .iter()
            .map(|d| d.predecessor_id)
            .collect();
        assert_eq!(predecessors, vec![b.id, a.id]);
    }

    #[test]
    fn add_dependency_should_reject_cycle_regardless_of_dependency_type() {
        let types = [
            DependencyType::FinishToStart,
            DependencyType::StartToStart,
            DependencyType::FinishToFinish,
            DependencyType::StartToFinish,
        ];
        for existing in types {
            for closing in types {
                let mut core = new_core();
                let a = core.create_task(minimal_new_task("A")).unwrap();
                let b = core.create_task(minimal_new_task("B")).unwrap();
                core.add_dependency(b.id, a.id, existing).unwrap();

                let result = core.add_dependency(a.id, b.id, closing);

                assert!(
                    matches!(result, Err(CoreError::CircularDependency { .. })),
                    "existing {existing:?}, closing {closing:?}: got {result:?}"
                );
            }
        }
    }

    #[test]
    fn add_dependency_should_reject_cycle_through_soft_deleted_task() {
        let mut core = new_core();
        let a = core.create_task(minimal_new_task("A")).unwrap();
        let b = core.create_task(minimal_new_task("B")).unwrap();
        let c = core.create_task(minimal_new_task("C")).unwrap();
        core.add_dependency(b.id, a.id, DependencyType::FinishToStart)
            .unwrap();
        core.add_dependency(c.id, b.id, DependencyType::FinishToStart)
            .unwrap();
        core.delete_task(b.id, DeleteMode::Subtree).unwrap();

        let result = core.add_dependency(a.id, c.id, DependencyType::FinishToStart);

        assert!(matches!(
            result,
            Err(CoreError::CircularDependency { cycle }) if cycle == vec![a.id, c.id, b.id, a.id]
        ));
        let stored = core.get_task(a.id).unwrap().unwrap();
        assert_eq!(stored.depends_on, []);
    }

    #[test]
    fn remove_dependency_should_drop_predecessor_from_depends_on() {
        let mut core = new_core();
        let predecessor = core.create_task(minimal_new_task("Predecessor")).unwrap();
        let task = core.create_task(minimal_new_task("Task")).unwrap();
        let with_edge = core
            .add_dependency(task.id, predecessor.id, DependencyType::FinishToStart)
            .unwrap();

        let updated = core.remove_dependency(task.id, predecessor.id).unwrap();

        assert_eq!(updated.id, task.id);
        assert_eq!(updated.depends_on, []);
        assert!(updated.updated_at >= with_edge.updated_at);
        let stored = core.get_task(task.id).unwrap().unwrap();
        assert_eq!(stored.depends_on, []);
        assert_eq!(stored.updated_at, updated.updated_at);
    }

    #[test]
    fn remove_dependency_should_keep_other_predecessors() {
        let mut core = new_core();
        let first = core.create_task(minimal_new_task("First")).unwrap();
        let second = core.create_task(minimal_new_task("Second")).unwrap();
        let task = core.create_task(minimal_new_task("Task")).unwrap();
        core.add_dependency(task.id, first.id, DependencyType::FinishToStart)
            .unwrap();
        core.add_dependency(task.id, second.id, DependencyType::StartToStart)
            .unwrap();

        let updated = core.remove_dependency(task.id, first.id).unwrap();

        let expected = vec![Dependency {
            predecessor_id: second.id,
            dep_type: DependencyType::StartToStart,
        }];
        assert_eq!(updated.depends_on, expected);
        let stored = core.get_task(task.id).unwrap().unwrap();
        assert_eq!(stored.depends_on, expected);
    }

    #[test]
    fn remove_dependency_should_reject_when_task_not_found() {
        let mut core = new_core();
        let predecessor = core.create_task(minimal_new_task("Predecessor")).unwrap();
        let missing = TaskId::new();

        let result = core.remove_dependency(missing, predecessor.id);

        assert!(matches!(result, Err(CoreError::NotFound(id)) if id == missing));
    }

    #[test]
    fn remove_dependency_should_reject_soft_deleted_task() {
        let mut core = new_core();
        let predecessor = core.create_task(minimal_new_task("Predecessor")).unwrap();
        let task = core.create_task(minimal_new_task("Task")).unwrap();
        core.add_dependency(task.id, predecessor.id, DependencyType::FinishToStart)
            .unwrap();
        core.delete_task(task.id, DeleteMode::Subtree).unwrap();

        let result = core.remove_dependency(task.id, predecessor.id);

        assert!(matches!(result, Err(CoreError::NotFound(id)) if id == task.id));
    }

    #[test]
    fn remove_dependency_should_change_nothing_when_no_such_edge_exists() {
        let store = CountingStore::new();
        let log = store.log();
        let mut core = Core::new(store).unwrap();
        let parent = core.create_task(minimal_new_task("Parent")).unwrap();
        let other = core.create_task(minimal_new_task("Other")).unwrap();
        let unrelated = core.create_task(minimal_new_task("Unrelated")).unwrap();
        let done_child = core
            .create_task(NewTask {
                parent_id: Some(parent.id),
                ..minimal_new_task("Done child")
            })
            .unwrap();
        core.create_task(NewTask {
            parent_id: Some(parent.id),
            ..minimal_new_task("Open child")
        })
        .unwrap();
        core.complete_task(done_child.id, false).unwrap();
        let before = core
            .add_dependency(parent.id, other.id, DependencyType::FinishToStart)
            .unwrap();
        let start = log.borrow().len();

        let returned = core.remove_dependency(parent.id, unrelated.id).unwrap();

        let calls = log.borrow()[start..].to_vec();
        assert!(
            calls
                .iter()
                .all(|&(_, m)| m != "put_task" && m != "remove_dependency_edge"),
            "calls: {calls:?}"
        );
        assert_eq!(returned, before);
        assert!((returned.progress - 0.5).abs() < f32::EPSILON);
        let stored = core.get_task(parent.id).unwrap().unwrap();
        assert_eq!(stored, before);
    }

    #[test]
    fn remove_dependency_should_work_for_a_soft_deleted_predecessor() {
        let mut core = new_core();
        let predecessor = core.create_task(minimal_new_task("Predecessor")).unwrap();
        let task = core.create_task(minimal_new_task("Task")).unwrap();
        core.add_dependency(task.id, predecessor.id, DependencyType::FinishToStart)
            .unwrap();
        core.delete_task(predecessor.id, DeleteMode::Subtree)
            .unwrap();

        let updated = core.remove_dependency(task.id, predecessor.id).unwrap();

        assert_eq!(updated.depends_on, []);
        let stored = core.get_task(task.id).unwrap().unwrap();
        assert_eq!(stored.depends_on, []);
    }

    #[test]
    fn remove_dependency_then_add_reverse_edge_should_succeed() {
        let mut core = new_core();
        let a = core.create_task(minimal_new_task("A")).unwrap();
        let b = core.create_task(minimal_new_task("B")).unwrap();
        core.add_dependency(b.id, a.id, DependencyType::FinishToStart)
            .unwrap();
        core.remove_dependency(b.id, a.id).unwrap();

        let updated = core
            .add_dependency(a.id, b.id, DependencyType::FinishToStart)
            .unwrap();

        assert_eq!(
            updated.depends_on,
            vec![Dependency {
                predecessor_id: b.id,
                dep_type: DependencyType::FinishToStart,
            }]
        );
    }
}
