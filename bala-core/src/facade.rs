//! The `Core` facade (LLD §Decision, §Method Contract): the single entry
//! point every caller (CLI today, Web API later) drives.
//!
//! Scoped to exactly what Story 1.1 needs: `create_task`, `get_tree`, and
//! thin `TaskType` pass-throughs. `set_parents`, dependency methods,
//! cascade, rollup, and `complete_task` belong to later stories/epics and
//! are deliberately absent.

use chrono::Utc;

use crate::error::CoreError;
use crate::hierarchy::check_new_parent;
use crate::model::{
    DeleteMode, Field, NewTask, Task, TaskId, TaskPatch, TaskStatus, TaskType, TreeFilter, User,
    UserId,
};
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

    /// Creates a user, so `Task::assignee_id` (added in a later checkpoint)
    /// has a real entity to resolve to.
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

    /// Creates a task per LLD §Algorithm (Story 1.1): validates the
    /// title, type key, date range, and each given parent, then persists
    /// the new task and its parent edges.
    ///
    /// # Errors
    ///
    /// - [`CoreError::EmptyTitle`] if `new.title` is empty or
    ///   whitespace-only.
    /// - [`CoreError::UnknownTaskType`] if `new.type_key` (or the
    ///   default) names no configured [`TaskType`].
    /// - [`CoreError::InvalidDateRange`] if both dates are given and
    ///   `due_date` is before `start_date`.
    /// - [`CoreError::NotFound`] if any of `new.parent_ids` names no
    ///   existing task.
    /// - [`CoreError::UnknownUser`] if `new.assignee_id` is `Some` and
    ///   names no existing [`User`].
    /// - [`CoreError::Store`] if the backend fails.
    pub fn create_task(&mut self, new: NewTask) -> Result<Task, CoreError> {
        if new.title.trim().is_empty() {
            return Err(CoreError::EmptyTitle);
        }

        let type_key = new.type_key.unwrap_or_else(|| DEFAULT_TYPE_KEY.to_owned());
        let types = self.store.transaction(|tx| tx.get_task_types())?;
        if !types.iter().any(|t| t.key == type_key) {
            return Err(CoreError::UnknownTaskType(type_key));
        }

        if let (Some(start), Some(due)) = (new.start_date, new.due_date)
            && due < start
        {
            return Err(CoreError::InvalidDateRange { start, due });
        }

        for &parent_id in &new.parent_ids {
            let parent_exists = self
                .store
                .transaction(|tx| tx.get_task(parent_id))?
                .is_some();
            if !parent_exists {
                return Err(CoreError::NotFound(parent_id));
            }
        }

        if let Some(assignee_id) = new.assignee_id {
            let assignee_exists = self
                .store
                .transaction(|tx| tx.get_user(assignee_id))?
                .is_some();
            if !assignee_exists {
                return Err(CoreError::UnknownUser(assignee_id));
            }
        }

        let now = Utc::now();
        let task = Task {
            id: TaskId::new(),
            title: new.title,
            description: new.description,
            parent_ids: new.parent_ids,
            type_key,
            status: TaskStatus::Incomplete,
            start_date: new.start_date,
            due_date: new.due_date,
            assignee_id: new.assignee_id,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            completed_at: None,
        };

        self.store.transaction(|tx| {
            tx.put_task(&task)?;
            for &parent_id in &task.parent_ids {
                check_new_parent(tx, parent_id, task.id)?;
                tx.add_parent_edge(parent_id, task.id)?;
            }
            Ok(())
        })?;

        Ok(task)
    }

    /// Lists tasks matching `filter`.
    ///
    /// Scoped to this checkpoint: a thin pass-through to
    /// `Store::list_tasks`, with no progress rollup (Story 3.3) or
    /// hierarchy assembly beyond what `Task::parent_ids` already carries.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend fails.
    // `TreeFilter` is taken by value to match the LLD §Method Contract
    // signature (`get_tree(&self, filter: TreeFilter)`), which callers
    // build fresh per call rather than reuse.
    #[allow(clippy::needless_pass_by_value)]
    pub fn get_tree(&self, filter: TreeFilter) -> Result<Vec<Task>, CoreError> {
        Ok(self.store.transaction(|tx| tx.list_tasks(&filter))?)
    }

    /// Lists every configured task type.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the backend fails.
    pub fn list_task_types(&self) -> Result<Vec<TaskType>, CoreError> {
        Ok(self.store.transaction(|tx| tx.get_task_types())?)
    }

    /// Updates an existing task per LLD §Algorithm (Story 1.2): applies
    /// `patch` field-by-field, validates the resulting state, bumps
    /// `updated_at`, and persists.
    ///
    /// `patch.title` and `patch.type_key` are `String`-backed, not
    /// `Option`-backed, on [`Task`], so [`Field::Clear`] on either is a
    /// defensive no-op equivalent to [`Field::Keep`] rather than a real
    /// path — no caller in this checkpoint constructs one.
    ///
    /// Scoped to exactly what Story 1.2 needs: no cascade rescheduling of
    /// dependents or subtasks (Epic 3) — a task's own dates and
    /// assignment change in isolation, and this always returns a
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

    /// Deletes a task per LLD §Algorithm (Story 1.3): tombstones `id`
    /// (`deleted_at` set, never a hard delete) and, per `mode`, either
    /// recursively tombstones every child left with no other live parent
    /// ([`DeleteMode::Subtree`]) or reparents each child onto `id`'s own
    /// parents, leaving it top-level if `id` had none
    /// ([`DeleteMode::PromoteChildren`]).
    ///
    /// With multiple parents, `Subtree` only ever removes parent-edges,
    /// not tasks: a child still reachable through another live parent
    /// keeps existing, just with one less parent edge, so deleting one
    /// goal can never silently delete a task another goal still needs.
    /// `PromoteChildren` follows the same rule in reverse: a child with
    /// other parents besides `id` simply gains an edge to each of `id`'s
    /// parents alongside the ones it already keeps.
    ///
    /// Everything runs inside one [`Store::transaction`]. Returns every
    /// [`Task`] actually touched — tombstoned, or (under
    /// `PromoteChildren`) reparented — so a caller can refresh its view
    /// from the return value alone.
    ///
    /// # Errors
    ///
    /// - [`CoreError::NotFound`] if `id` names no existing, live task —
    ///   including a task that is already soft-deleted, which is treated
    ///   as not-found rather than tombstoned a second time.
    /// - [`CoreError::Store`] if the backend fails.
    pub fn delete_task(&mut self, id: TaskId, mode: DeleteMode) -> Result<Vec<Task>, CoreError> {
        let touched = self.store.transaction(|tx| {
            // `get_task` (not `get_task_including_deleted`) so a task
            // that's already soft-deleted is treated as not-found here,
            // matching `update_task`'s "not found" idiom rather than
            // re-tombstoning/double-counting it.
            let Some(task) = tx.get_task(id)? else {
                return Ok(Err(CoreError::NotFound(id)));
            };

            let now = Utc::now();
            let mut touched = Vec::new();

            match mode {
                DeleteMode::Subtree => {
                    tombstone_subtree(tx, id, now, &mut touched)?;
                }
                DeleteMode::PromoteChildren => {
                    // `id`'s own parents, fetched independent of the
                    // child loop below: they're a separate edge set
                    // (id-as-child-of-its-parents) from the edges the
                    // loop mutates (id-as-parent-of-its-children).
                    let grandparents = tx.list_parent_edges(id)?;

                    let mut deleted = task;
                    deleted.deleted_at = Some(now);
                    deleted.updated_at = now;
                    tx.put_task(&deleted)?;
                    touched.push(deleted);

                    for child in tx.list_child_edges(id)? {
                        tx.remove_parent_edge(id, child)?;
                        for &grandparent in &grandparents {
                            tx.add_parent_edge(grandparent, child)?;
                        }

                        if let Some(mut child_task) = tx.get_task(child)? {
                            child_task.parent_ids.retain(|&p| p != id);
                            for &grandparent in &grandparents {
                                if !child_task.parent_ids.contains(&grandparent) {
                                    child_task.parent_ids.push(grandparent);
                                }
                            }
                            child_task.updated_at = now;
                            tx.put_task(&child_task)?;
                            touched.push(child_task);
                        }
                    }
                }
            }

            Ok(Ok(touched))
        })??;

        Ok(touched)
    }

    /// Restores a soft-deleted task per LLD §Algorithm (Story 1.3, AC5):
    /// looks `id` up via [`StoreTx::get_task_including_deleted`] — unlike
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
            task.updated_at = Utc::now();
            tx.put_task(&task)?;

            Ok(Ok(task))
        })??;

        Ok(task)
    }

    /// Completes a task per LLD §Algorithm 5 (Story 1.4): marks `id`
    /// `status = Complete`, `completed_at = Some(now)`, `updated_at = now`.
    ///
    /// If `id` has any direct child whose `status` is still
    /// [`TaskStatus::Incomplete`], the call is rejected with
    /// [`CoreError::IncompleteChildren`] and nothing is written — unless
    /// `cascade` is `true`, in which case `id`'s entire subtree is marked
    /// complete instead (every not-yet-complete descendant, walked
    /// iteratively so an arbitrarily deep hierarchy can't overflow the
    /// stack).
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
    /// - [`CoreError::IncompleteChildren`] if `id` has a direct child that
    ///   is still incomplete and `cascade` is `false`.
    /// - [`CoreError::Store`] if the backend fails.
    pub fn complete_task(&mut self, id: TaskId, cascade: bool) -> Result<Vec<Task>, CoreError> {
        let touched = self.store.transaction(|tx| {
            // Fetched once and reused below (non-cascade branch) rather than
            // re-fetched, since it's the same row `Store::get_task` already
            // returned for the existence check.
            let Some(task) = tx.get_task(id)? else {
                return Ok(Err(CoreError::NotFound(id)));
            };

            let incomplete_children = direct_incomplete_children(tx, id)?;
            if !incomplete_children.is_empty() && !cascade {
                return Ok(Err(CoreError::IncompleteChildren {
                    task: id,
                    incomplete: incomplete_children,
                }));
            }

            let now = Utc::now();
            let mut touched = Vec::new();
            if cascade {
                mark_complete_subtree(tx, id, now, &mut touched)?;
            } else if task.status != TaskStatus::Complete {
                let mut task = task;
                task.status = TaskStatus::Complete;
                task.completed_at = Some(now);
                task.updated_at = now;
                tx.put_task(&task)?;
                touched.push(task);
            }

            Ok(Ok(touched))
        })??;

        Ok(touched)
    }
}

/// Collects the ids of every direct child of `id` whose `status` is still
/// [`TaskStatus::Incomplete`]. A child id that no longer resolves via
/// [`StoreTx::get_task`] (already deleted) is skipped, matching how other
/// code in this module treats missing lookups.
fn direct_incomplete_children(tx: &mut dyn StoreTx, id: TaskId) -> Result<Vec<TaskId>, StoreError> {
    let mut incomplete = Vec::new();
    for child in tx.list_child_edges(id)? {
        if let Some(child_task) = tx.get_task(child)?
            && child_task.status == TaskStatus::Incomplete
        {
            incomplete.push(child);
        }
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
/// `updated_at`. A task reachable through two parents is visited only
/// once (tracked via `visited`), and a task already
/// [`TaskStatus::Complete`] is walked through (to reach further
/// descendants) but not re-written or added to `touched`, so its
/// `completed_at` isn't clobbered and it isn't double-counted.
fn mark_complete_subtree(
    tx: &mut dyn StoreTx,
    id: TaskId,
    now: chrono::DateTime<Utc>,
    touched: &mut Vec<Task>,
) -> Result<(), StoreError> {
    let mut visited = std::collections::HashSet::new();
    let mut pending = vec![id];

    while let Some(current) = pending.pop() {
        if !visited.insert(current) {
            continue;
        }

        let Some(mut task) = tx.get_task(current)? else {
            continue;
        };

        if task.status != TaskStatus::Complete {
            task.status = TaskStatus::Complete;
            task.completed_at = Some(now);
            task.updated_at = now;
            tx.put_task(&task)?;
            touched.push(task);
        }

        pending.extend(tx.list_child_edges(current)?);
    }

    Ok(())
}

/// Tombstones `id` under [`DeleteMode::Subtree`]: sets
/// `deleted_at`/`updated_at`, drops the `id -> child` edge for every
/// direct child, and — only for a child left with no remaining parents —
/// processes it too, at arbitrary depth. A child still reachable through
/// another live parent keeps existing untouched beyond that one dropped
/// edge.
///
/// Iterative (an explicit work stack), not recursive: `create_task`
/// permits arbitrarily deep parent chains, so a user-built hierarchy deep
/// enough could overflow the process stack if this walked it via Rust
/// call recursion instead.
///
/// Every tombstoned (and, for a surviving child, edge-updated) [`Task`] is
/// appended to `touched`.
fn tombstone_subtree(
    tx: &mut dyn StoreTx,
    id: TaskId,
    now: chrono::DateTime<Utc>,
    touched: &mut Vec<Task>,
) -> Result<(), StoreError> {
    let mut pending = vec![id];

    while let Some(current) = pending.pop() {
        let Some(mut task) = tx.get_task_including_deleted(current)? else {
            continue;
        };
        task.deleted_at = Some(now);
        task.updated_at = now;
        tx.put_task(&task)?;
        touched.push(task);

        for child in tx.list_child_edges(current)? {
            tx.remove_parent_edge(current, child)?;
            let remaining_parents = tx.list_parent_edges(child)?;

            if remaining_parents.is_empty() {
                pending.push(child);
            } else if let Some(mut child_task) = tx.get_task(child)? {
                child_task.parent_ids.retain(|&p| p != current);
                child_task.updated_at = now;
                tx.put_task(&child_task)?;
                touched.push(child_task);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::in_memory_store::InMemoryStore;
    use crate::model::TaskStatus;
    use chrono::NaiveDate;

    fn new_core() -> Core<InMemoryStore> {
        Core::new(InMemoryStore::default()).unwrap()
    }

    fn minimal_new_task(title: &str) -> NewTask {
        NewTask {
            title: title.to_owned(),
            description: None,
            parent_ids: Vec::new(),
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

        assert!(task.parent_ids.is_empty());
    }

    #[test]
    fn create_task_should_attach_to_given_parent_when_parent_ids_provided() {
        let mut core = new_core();
        let parent = core.create_task(minimal_new_task("Parent")).unwrap();

        let child = core
            .create_task(NewTask {
                parent_ids: vec![parent.id],
                ..minimal_new_task("Child")
            })
            .unwrap();

        assert_eq!(child.parent_ids, vec![parent.id]);
    }

    #[test]
    fn create_task_should_reject_when_given_parent_does_not_exist() {
        let mut core = new_core();
        let missing_parent = TaskId::new();

        let result = core.create_task(NewTask {
            parent_ids: vec![missing_parent],
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

    #[test]
    fn create_task_should_run_hierarchy_check_for_each_given_parent() {
        // The hierarchy check is a no-op at this checkpoint (Story 3.1
        // gives it teeth), so the honest thing to assert here is that
        // attaching a task under several parents in one call still
        // succeeds and records every edge — i.e. the per-parent check
        // (`hierarchy::check_new_parent`) runs without rejecting any of
        // them. `hierarchy::tests::check_new_parent_always_succeeds_for_a_fresh_child`
        // separately unit-tests the function itself.
        let mut core = new_core();
        let parent_a = core.create_task(minimal_new_task("Parent A")).unwrap();
        let parent_b = core.create_task(minimal_new_task("Parent B")).unwrap();

        let child = core
            .create_task(NewTask {
                parent_ids: vec![parent_a.id, parent_b.id],
                ..minimal_new_task("Multi-parent child")
            })
            .unwrap();

        assert_eq!(child.parent_ids, vec![parent_a.id, parent_b.id]);
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
                parent_ids: vec![parent.id],
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

        let touched = core.delete_task(task.id, DeleteMode::Subtree).unwrap();

        assert_eq!(touched.len(), 1);
        assert_eq!(touched[0].id, task.id);
        assert!(touched[0].deleted_at.is_some());

        // A second delete on the same (now soft-deleted) task is treated
        // as not-found, not re-tombstoned.
        let result = core.delete_task(task.id, DeleteMode::Subtree);
        assert!(matches!(result, Err(CoreError::NotFound(id)) if id == task.id));
    }

    #[test]
    fn delete_task_subtree_should_tombstone_child_with_no_other_parents() {
        let mut core = new_core();
        let parent = core.create_task(minimal_new_task("Parent")).unwrap();
        let child = core
            .create_task(NewTask {
                parent_ids: vec![parent.id],
                ..minimal_new_task("Child")
            })
            .unwrap();

        core.delete_task(parent.id, DeleteMode::Subtree).unwrap();

        let tree = core
            .get_tree(TreeFilter {
                include_deleted: true,
                ..TreeFilter::default()
            })
            .unwrap();
        let child_after = tree.iter().find(|t| t.id == child.id).unwrap();
        assert!(child_after.deleted_at.is_some());
    }

    #[test]
    fn delete_subtree_should_keep_child_reachable_through_other_parent() {
        let mut core = new_core();
        let parent_a = core.create_task(minimal_new_task("Goal A")).unwrap();
        let parent_b = core.create_task(minimal_new_task("Goal B")).unwrap();
        let child = core
            .create_task(NewTask {
                parent_ids: vec![parent_a.id, parent_b.id],
                ..minimal_new_task("Shared child")
            })
            .unwrap();

        core.delete_task(parent_a.id, DeleteMode::Subtree).unwrap();

        let tree = core.get_tree(TreeFilter::default()).unwrap();
        let child_after = tree.iter().find(|t| t.id == child.id).unwrap();
        assert!(child_after.deleted_at.is_none());
        assert_eq!(child_after.parent_ids, vec![parent_b.id]);
    }

    #[test]
    fn delete_task_subtree_should_recurse_through_multiple_levels() {
        let mut core = new_core();
        let root = core.create_task(minimal_new_task("Root")).unwrap();
        let mid = core
            .create_task(NewTask {
                parent_ids: vec![root.id],
                ..minimal_new_task("Mid")
            })
            .unwrap();
        let leaf = core
            .create_task(NewTask {
                parent_ids: vec![mid.id],
                ..minimal_new_task("Leaf")
            })
            .unwrap();
        let deepest = core
            .create_task(NewTask {
                parent_ids: vec![leaf.id],
                ..minimal_new_task("Deepest")
            })
            .unwrap();

        let touched = core.delete_task(root.id, DeleteMode::Subtree).unwrap();

        let touched_ids: std::collections::HashSet<_> = touched.iter().map(|t| t.id).collect();
        assert!(touched_ids.contains(&root.id));
        assert!(touched_ids.contains(&mid.id));
        assert!(touched_ids.contains(&leaf.id));
        assert!(touched_ids.contains(&deepest.id));

        let tree = core
            .get_tree(TreeFilter {
                include_deleted: true,
                ..TreeFilter::default()
            })
            .unwrap();
        for id in [root.id, mid.id, leaf.id, deepest.id] {
            let task = tree.iter().find(|t| t.id == id).unwrap();
            assert!(task.deleted_at.is_some(), "{id:?} should be tombstoned");
        }
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
                    parent_ids: vec![current],
                    ..minimal_new_task(&format!("Level {i}"))
                })
                .unwrap();
            current = task.id;
        }

        let touched = core.delete_task(root.id, DeleteMode::Subtree).unwrap();

        assert_eq!(touched.len(), 5001);
        assert!(touched.iter().all(|t| t.deleted_at.is_some()));
    }

    #[test]
    fn delete_task_promote_children_should_reparent_child_to_deleted_tasks_parents() {
        let mut core = new_core();
        let grandparent = core.create_task(minimal_new_task("Grandparent")).unwrap();
        let parent = core
            .create_task(NewTask {
                parent_ids: vec![grandparent.id],
                ..minimal_new_task("Parent")
            })
            .unwrap();
        let child = core
            .create_task(NewTask {
                parent_ids: vec![parent.id],
                ..minimal_new_task("Child")
            })
            .unwrap();

        core.delete_task(parent.id, DeleteMode::PromoteChildren)
            .unwrap();

        let tree = core.get_tree(TreeFilter::default()).unwrap();
        let child_after = tree.iter().find(|t| t.id == child.id).unwrap();
        assert_eq!(child_after.parent_ids, vec![grandparent.id]);
    }

    #[test]
    fn delete_task_promote_children_should_make_child_top_level_when_deleted_task_had_no_parents() {
        let mut core = new_core();
        let parent = core.create_task(minimal_new_task("Parent")).unwrap();
        let child = core
            .create_task(NewTask {
                parent_ids: vec![parent.id],
                ..minimal_new_task("Child")
            })
            .unwrap();

        core.delete_task(parent.id, DeleteMode::PromoteChildren)
            .unwrap();

        let tree = core.get_tree(TreeFilter::default()).unwrap();
        let child_after = tree.iter().find(|t| t.id == child.id).unwrap();
        assert!(child_after.parent_ids.is_empty());
    }

    #[test]
    fn delete_task_promote_children_should_add_parent_alongside_existing_other_parents() {
        let mut core = new_core();
        let grandparent = core.create_task(minimal_new_task("Grandparent")).unwrap();
        let parent = core
            .create_task(NewTask {
                parent_ids: vec![grandparent.id],
                ..minimal_new_task("Parent")
            })
            .unwrap();
        let other_parent = core.create_task(minimal_new_task("Other parent")).unwrap();
        let child = core
            .create_task(NewTask {
                parent_ids: vec![parent.id, other_parent.id],
                ..minimal_new_task("Child")
            })
            .unwrap();

        core.delete_task(parent.id, DeleteMode::PromoteChildren)
            .unwrap();

        let tree = core.get_tree(TreeFilter::default()).unwrap();
        let child_after = tree.iter().find(|t| t.id == child.id).unwrap();
        let parents: std::collections::HashSet<_> =
            child_after.parent_ids.iter().copied().collect();
        let expected: std::collections::HashSet<_> =
            [other_parent.id, grandparent.id].into_iter().collect();
        assert_eq!(parents, expected);
    }

    #[test]
    fn delete_task_promote_children_should_not_duplicate_parent_edge_when_child_already_shares_a_grandparent()
     {
        // Regression test: `child` already has `grandparent` as a parent
        // (alongside `parent`, which is being deleted) — `PromoteChildren`
        // then tries to add a `grandparent -> child` edge that already
        // exists. `add_parent_edge` must treat that as a no-op rather than
        // a duplicate entry, so `parent_ids` still names `grandparent`
        // exactly once afterward.
        let mut core = new_core();
        let grandparent = core.create_task(minimal_new_task("Grandparent")).unwrap();
        let parent = core
            .create_task(NewTask {
                parent_ids: vec![grandparent.id],
                ..minimal_new_task("Parent")
            })
            .unwrap();
        let child = core
            .create_task(NewTask {
                parent_ids: vec![parent.id, grandparent.id],
                ..minimal_new_task("Child")
            })
            .unwrap();

        core.delete_task(parent.id, DeleteMode::PromoteChildren)
            .unwrap();

        let tree = core.get_tree(TreeFilter::default()).unwrap();
        let child_after = tree.iter().find(|t| t.id == child.id).unwrap();
        assert_eq!(child_after.parent_ids, vec![grandparent.id]);
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
    fn delete_task_should_return_every_task_actually_touched() {
        let mut core = new_core();
        let parent_a = core.create_task(minimal_new_task("Parent A")).unwrap();
        let parent_b = core.create_task(minimal_new_task("Parent B")).unwrap();
        let shared_child = core
            .create_task(NewTask {
                parent_ids: vec![parent_a.id, parent_b.id],
                ..minimal_new_task("Shared child")
            })
            .unwrap();
        let only_child = core
            .create_task(NewTask {
                parent_ids: vec![parent_a.id],
                ..minimal_new_task("Only child")
            })
            .unwrap();

        let touched = core.delete_task(parent_a.id, DeleteMode::Subtree).unwrap();

        let touched_ids: std::collections::HashSet<_> = touched.iter().map(|t| t.id).collect();
        assert!(touched_ids.contains(&parent_a.id));
        assert!(touched_ids.contains(&only_child.id));
        assert!(touched_ids.contains(&shared_child.id));
        assert!(!touched_ids.contains(&parent_b.id));
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
    fn complete_task_should_block_when_children_incomplete_and_cascade_false() {
        let mut core = new_core();
        let parent = core.create_task(minimal_new_task("Parent")).unwrap();
        let child = core
            .create_task(NewTask {
                parent_ids: vec![parent.id],
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
    fn complete_task_should_cascade_complete_whole_subtree_when_cascade_true() {
        let mut core = new_core();
        let root = core.create_task(minimal_new_task("Root")).unwrap();
        let mid = core
            .create_task(NewTask {
                parent_ids: vec![root.id],
                ..minimal_new_task("Mid")
            })
            .unwrap();
        let leaf = core
            .create_task(NewTask {
                parent_ids: vec![mid.id],
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
                parent_ids: vec![parent.id],
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
                parent_ids: vec![root.id],
                ..minimal_new_task("Child A")
            })
            .unwrap();
        let child_b = core
            .create_task(NewTask {
                parent_ids: vec![root.id],
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

        assert!(second.is_empty());
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
                    parent_ids: vec![current],
                    ..minimal_new_task(&format!("Level {i}"))
                })
                .unwrap();
            current = task.id;
        }

        let touched = core.complete_task(root.id, true).unwrap();

        assert_eq!(touched.len(), 5001);
        assert!(touched.iter().all(|t| t.status == TaskStatus::Complete));
    }
}
