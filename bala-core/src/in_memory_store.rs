//! An in-memory [`Store`]/[`StoreTx`] implementation for tests.
//!
//! Not a production backend — no persistence, no concurrency control
//! beyond a single [`RefCell`]. It exists so `bala-core`'s algorithms (and
//! this module's own put/get/list/edge round-trip tests) can run
//! without a real database.
//!
//! Parent edges are kept in two maps mirroring the SQLite `parent_edges`
//! table: child -> its one parent (`None` = the NULL top-level edge) and
//! parent -> children in position order (`None` = the root list). A child's
//! position is its index in the parent's list.
//!
//! Dependency edges are likewise kept in two maps mirroring the SQLite
//! `dependency_edges` table: successor -> its predecessors (each with the
//! edge's type) and predecessor -> its successors, both in the order the
//! edges were first added.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::model::{
    Dependency, DependencyType, Placement, Task, TaskId, TaskType, TreeFilter, User, UserId,
};
use crate::store::{Store, StoreError, StoreTx};

/// Test double for [`Store`], backed by in-memory maps.
#[derive(Debug, Default)]
pub struct InMemoryStore {
    inner: RefCell<State>,
}

#[derive(Debug, Default)]
struct State {
    users: HashMap<UserId, User>,
    tasks: HashMap<TaskId, Task>,
    /// child -> its parent; `None` is a top-level task's NULL edge. A
    /// child with no entry has no edge at all.
    parent_of: HashMap<TaskId, Option<TaskId>>,
    /// parent (`None` = the root list) -> children, in position order.
    children_of: HashMap<Option<TaskId>, Vec<TaskId>>,
    /// successor -> the tasks it depends on, in insertion order.
    predecessors_of: HashMap<TaskId, Vec<Dependency>>,
    /// predecessor -> the tasks that depend on it, in insertion order.
    successors_of: HashMap<TaskId, Vec<TaskId>>,
    task_types: HashMap<String, TaskType>,
}

impl Store for InMemoryStore {
    fn transaction<T>(
        &self,
        f: impl FnOnce(&mut dyn StoreTx) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let mut state = self.inner.borrow_mut();
        f(&mut *state)
    }
}

impl State {
    /// Clones a stored task as a read returns it: `parent_id` is taken from
    /// the parent edge and `depends_on` from the dependency edges, whatever
    /// values `put_task` was handed. `progress` and `out_of_sync` come back
    /// as the `0.0` and `false` that `put_task` stored.
    fn read_task(&self, task: &Task) -> Task {
        let mut task = task.clone();
        task.parent_id = self.parent_of.get(&task.id).copied().flatten();
        task.depends_on = self
            .predecessors_of
            .get(&task.id)
            .cloned()
            .unwrap_or_default();
        task
    }
}

impl StoreTx for State {
    fn get_user(&mut self, id: UserId) -> Result<Option<User>, StoreError> {
        Ok(self.users.get(&id).cloned())
    }

    fn put_user(&mut self, user: &User) -> Result<(), StoreError> {
        self.users.insert(user.id, user.clone());
        Ok(())
    }

    fn list_users(&mut self) -> Result<Vec<User>, StoreError> {
        Ok(self.users.values().cloned().collect())
    }

    fn get_task(&mut self, id: TaskId) -> Result<Option<Task>, StoreError> {
        Ok(self
            .tasks
            .get(&id)
            .filter(|task| task.deleted_at.is_none())
            .map(|task| self.read_task(task)))
    }

    fn put_task(&mut self, task: &Task) -> Result<(), StoreError> {
        // `progress` and `out_of_sync` are library-computed, never
        // persisted (`bala-store`'s SQLite backend has no column for either
        // and always reconstructs `0.0` and `false` on read) — reset them
        // here too, so a caller driving `StoreTx` directly sees the same
        // non-persistence behavior regardless of which `Store` backend is
        // live. `Core` always recomputes the real values before returning a
        // `Task` to its own callers, so this only matters to a caller
        // bypassing `Core`.
        let mut task = task.clone();
        task.progress = 0.0;
        task.out_of_sync = false;
        self.tasks.insert(task.id, task);
        Ok(())
    }

    fn list_tasks(&mut self, filter: &TreeFilter) -> Result<Vec<Task>, StoreError> {
        Ok(self
            .tasks
            .values()
            .filter(|task| {
                filter
                    .type_key
                    .as_deref()
                    .is_none_or(|type_key| task.type_key == type_key)
            })
            .filter(|task| filter.status.is_none_or(|status| task.status == status))
            .filter(|task| {
                filter
                    .assignee_id
                    .is_none_or(|assignee_id| task.assignee_id == Some(assignee_id))
            })
            .filter(|task| filter.include_deleted || task.deleted_at.is_none())
            .map(|task| self.read_task(task))
            .collect())
    }

    fn get_parent_edge(&mut self, id: TaskId) -> Result<Option<TaskId>, StoreError> {
        Ok(self.parent_of.get(&id).copied().flatten())
    }

    fn set_parent_edge(
        &mut self,
        child: TaskId,
        parent: Option<TaskId>,
        placement: Placement,
    ) -> Result<(), StoreError> {
        match self.parent_of.insert(child, parent) {
            // Already under `parent`: the edge keeps its position.
            Some(old) if old == parent => return Ok(()),
            Some(old) => {
                if let Some(siblings) = self.children_of.get_mut(&old) {
                    siblings.retain(|&c| c != child);
                }
            }
            None => {}
        }
        let siblings = self.children_of.entry(parent).or_default();
        let at = match placement {
            Placement::After(sibling) if sibling != child => siblings
                .iter()
                .position(|&c| c == sibling)
                .map_or(siblings.len(), |i| i + 1),
            _ => siblings.len(),
        };
        siblings.insert(at, child);
        Ok(())
    }

    fn swap_child_positions(
        &mut self,
        parent: Option<TaskId>,
        a: TaskId,
        b: TaskId,
    ) -> Result<(), StoreError> {
        if let Some(siblings) = self.children_of.get_mut(&parent)
            && let (Some(i), Some(j)) = (
                siblings.iter().position(|&c| c == a),
                siblings.iter().position(|&c| c == b),
            )
        {
            siblings.swap(i, j);
        }
        Ok(())
    }

    fn list_child_edges(&mut self, parent: Option<TaskId>) -> Result<Vec<TaskId>, StoreError> {
        Ok(self.children_of.get(&parent).cloned().unwrap_or_default())
    }

    fn list_all_child_edges(&mut self) -> Result<Vec<(Option<TaskId>, TaskId)>, StoreError> {
        Ok(self
            .children_of
            .iter()
            .flat_map(|(&parent, kids)| kids.iter().map(move |&c| (parent, c)))
            .collect())
    }

    fn list_dependency_edges(&mut self, id: TaskId) -> Result<Vec<Dependency>, StoreError> {
        Ok(self.predecessors_of.get(&id).cloned().unwrap_or_default())
    }

    fn list_successor_edges(&mut self, id: TaskId) -> Result<Vec<TaskId>, StoreError> {
        Ok(self.successors_of.get(&id).cloned().unwrap_or_default())
    }

    fn add_dependency_edge(
        &mut self,
        predecessor: TaskId,
        successor: TaskId,
        dep_type: DependencyType,
    ) -> Result<(), StoreError> {
        let predecessors = self.predecessors_of.entry(successor).or_default();
        if let Some(existing) = predecessors
            .iter_mut()
            .find(|dep| dep.predecessor_id == predecessor)
        {
            // An existing pair keeps its place in both lists.
            existing.dep_type = dep_type;
        } else {
            predecessors.push(Dependency {
                predecessor_id: predecessor,
                dep_type,
            });
            self.successors_of
                .entry(predecessor)
                .or_default()
                .push(successor);
        }
        Ok(())
    }

    fn remove_dependency_edge(
        &mut self,
        predecessor: TaskId,
        successor: TaskId,
    ) -> Result<(), StoreError> {
        if let Some(predecessors) = self.predecessors_of.get_mut(&successor) {
            predecessors.retain(|dep| dep.predecessor_id != predecessor);
        }
        if let Some(successors) = self.successors_of.get_mut(&predecessor) {
            successors.retain(|&s| s != successor);
        }
        Ok(())
    }

    fn get_task_including_deleted(&mut self, id: TaskId) -> Result<Option<Task>, StoreError> {
        Ok(self.tasks.get(&id).map(|task| self.read_task(task)))
    }

    fn get_task_types(&mut self) -> Result<Vec<TaskType>, StoreError> {
        Ok(self.task_types.values().cloned().collect())
    }

    fn put_task_type(&mut self, t: &TaskType) -> Result<(), StoreError> {
        self.task_types.insert(t.key.clone(), t.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TaskStatus;
    use chrono::Utc;
    use uuid::Uuid;

    fn sample_task(type_key: &str, status: TaskStatus) -> Task {
        let now = Utc::now();
        Task {
            id: TaskId::new(),
            title: "Title".to_owned(),
            description: None,
            parent_id: None,
            type_key: type_key.to_owned(),
            status,
            // `InMemoryStore::put_task` always resets `progress` to `0.0`
            // on write, matching `bala-store`'s non-persistence — so a
            // round-tripped `Task` never carries this value back, and
            // building it as anything but `0.0` here would make every
            // `fetched == Some(task)` equality assertion below fail.
            progress: 0.0,
            start_date: None,
            due_date: None,
            duration_days: None,
            dates_fixed: false,
            assignee_id: None,
            depends_on: Vec::new(),
            out_of_sync: false,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            completed_at: None,
        }
    }

    #[test]
    fn put_task_then_get_task_returns_same_task() {
        let store = InMemoryStore::default();
        let task = sample_task("task", TaskStatus::Incomplete);

        store
            .transaction(|tx| {
                tx.put_task(&task)?;
                Ok(())
            })
            .unwrap();

        let fetched = store.transaction(|tx| tx.get_task(task.id)).unwrap();
        assert_eq!(fetched, Some(task));
    }

    #[test]
    fn put_task_then_get_task_round_trips_duration_and_dates_fixed() {
        let store = InMemoryStore::default();
        let mut task = sample_task("task", TaskStatus::Incomplete);
        task.start_date = chrono::NaiveDate::from_ymd_opt(2026, 1, 1);
        task.duration_days = Some(3);
        task.dates_fixed = true;
        let mut milestone = sample_task("task", TaskStatus::Incomplete);
        milestone.duration_days = Some(0);

        store
            .transaction(|tx| {
                tx.put_task(&task)?;
                tx.put_task(&milestone)?;
                Ok(())
            })
            .unwrap();

        let fetched = store.transaction(|tx| tx.get_task(task.id)).unwrap();
        assert_eq!(fetched, Some(task));
        let fetched = store.transaction(|tx| tx.get_task(milestone.id)).unwrap();
        assert_eq!(fetched, Some(milestone));
    }

    #[test]
    fn put_task_should_not_persist_out_of_sync() {
        let store = InMemoryStore::default();
        let mut task = sample_task("task", TaskStatus::Incomplete);
        task.out_of_sync = true;

        store.transaction(|tx| tx.put_task(&task)).unwrap();

        let fetched = store.transaction(|tx| tx.get_task(task.id)).unwrap();
        assert!(!fetched.unwrap().out_of_sync);
    }

    #[test]
    fn get_task_for_unknown_id_returns_none() {
        let store = InMemoryStore::default();
        let fetched = store.transaction(|tx| tx.get_task(TaskId::new())).unwrap();
        assert_eq!(fetched, None);
    }

    #[test]
    fn list_tasks_with_no_filter_returns_all_tasks() {
        let store = InMemoryStore::default();
        let a = sample_task("task", TaskStatus::Incomplete);
        let b = sample_task("goal", TaskStatus::Complete);

        store
            .transaction(|tx| {
                tx.put_task(&a)?;
                tx.put_task(&b)?;
                Ok(())
            })
            .unwrap();

        let mut listed = store
            .transaction(|tx| tx.list_tasks(&TreeFilter::default()))
            .unwrap();
        listed.sort_by_key(|t| Uuid::from(t.id));
        let mut expected = vec![a, b];
        expected.sort_by_key(|t| Uuid::from(t.id));
        assert_eq!(listed, expected);
    }

    #[test]
    fn list_tasks_filters_by_type_key() {
        let store = InMemoryStore::default();
        let goal = sample_task("goal", TaskStatus::Incomplete);
        let task = sample_task("task", TaskStatus::Incomplete);

        store
            .transaction(|tx| {
                tx.put_task(&goal)?;
                tx.put_task(&task)?;
                Ok(())
            })
            .unwrap();

        let filter = TreeFilter {
            type_key: Some("goal".to_owned()),
            ..TreeFilter::default()
        };
        let listed = store.transaction(|tx| tx.list_tasks(&filter)).unwrap();
        assert_eq!(listed, vec![goal]);
    }

    #[test]
    fn list_tasks_filters_by_status() {
        let store = InMemoryStore::default();
        let done = sample_task("task", TaskStatus::Complete);
        let not_done = sample_task("task", TaskStatus::Incomplete);

        store
            .transaction(|tx| {
                tx.put_task(&done)?;
                tx.put_task(&not_done)?;
                Ok(())
            })
            .unwrap();

        let filter = TreeFilter {
            status: Some(TaskStatus::Complete),
            ..TreeFilter::default()
        };
        let listed = store.transaction(|tx| tx.list_tasks(&filter)).unwrap();
        assert_eq!(listed, vec![done]);
    }

    #[test]
    fn get_parent_edge_for_task_with_no_parent_returns_none() {
        let store = InMemoryStore::default();
        let edge = store
            .transaction(|tx| tx.get_parent_edge(TaskId::new()))
            .unwrap();
        assert_eq!(edge, None);
    }

    #[test]
    fn set_parent_edge_then_get_parent_edge_returns_it() {
        let store = InMemoryStore::default();
        let parent = TaskId::new();
        let child = TaskId::new();

        store
            .transaction(|tx| tx.set_parent_edge(child, Some(parent), Placement::End))
            .unwrap();

        let edge = store.transaction(|tx| tx.get_parent_edge(child)).unwrap();
        assert_eq!(edge, Some(parent));
    }

    #[test]
    #[allow(clippy::many_single_char_names)]
    fn set_parent_edge_should_replace_the_previous_parent() {
        let store = InMemoryStore::default();
        let v = ids(6);
        let (p, q, a, child, b, c) = (v[0], v[1], v[2], v[3], v[4], v[5]);
        store
            .transaction(|tx| {
                append(tx, a, Some(p), Placement::End);
                append(tx, child, Some(p), Placement::End);
                append(tx, b, Some(p), Placement::End);
                append(tx, c, Some(q), Placement::End);
                Ok(())
            })
            .unwrap();

        store
            .transaction(|tx| tx.set_parent_edge(child, Some(q), Placement::End))
            .unwrap();

        // One edge, to the new parent: the old one is gone, the former
        // siblings keep their order, and the child lands at the end.
        let children = |parent| store.transaction(|tx| tx.list_child_edges(parent)).unwrap();
        let parent_of = |id| store.transaction(|tx| tx.get_parent_edge(id)).unwrap();
        assert_eq!(parent_of(child), Some(q));
        assert_eq!(children(Some(p)), [a, b]);
        assert_eq!(children(Some(q)), [c, child]);

        // Moving to the top level replaces the real edge with the NULL one,
        // and moving back under a parent replaces the NULL one.
        store
            .transaction(|tx| tx.set_parent_edge(child, None, Placement::End))
            .unwrap();
        assert_eq!(parent_of(child), None);
        assert_eq!(children(Some(q)), [c]);
        assert_eq!(children(None), [child]);
        store
            .transaction(|tx| tx.set_parent_edge(child, Some(p), Placement::After(a)))
            .unwrap();
        assert_eq!(parent_of(child), Some(p));
        assert_eq!(children(None), []);
        assert_eq!(children(Some(p)), [a, child, b]);
    }

    #[test]
    fn get_task_should_take_parent_id_from_the_parent_edge() {
        let store = InMemoryStore::default();
        let (parent, other) = (TaskId::new(), TaskId::new());
        let mut task = sample_task("task", TaskStatus::Incomplete);
        task.parent_id = Some(other);
        let id = task.id;
        store
            .transaction(|tx| {
                tx.put_task(&task)?;
                tx.set_parent_edge(id, Some(parent), Placement::End)
            })
            .unwrap();

        // The edge, not the value `put_task` was handed, is what a read
        // reports, and it follows the edge when the edge moves.
        let read = |id| {
            let filter = TreeFilter::default();
            store
                .transaction(|tx| {
                    Ok((
                        tx.get_task(id)?.unwrap().parent_id,
                        tx.get_task_including_deleted(id)?.unwrap().parent_id,
                        tx.list_tasks(&filter)?[0].parent_id,
                    ))
                })
                .unwrap()
        };
        assert_eq!(read(id), (Some(parent), Some(parent), Some(parent)));
        store
            .transaction(|tx| tx.set_parent_edge(id, None, Placement::End))
            .unwrap();
        assert_eq!(read(id), (None, None, None));
    }

    #[test]
    fn top_level_task_should_have_single_null_parent_edge() {
        let mut store = State::default();
        let a = TaskId::new();
        store.set_parent_edge(a, None, Placement::End).unwrap();
        store.set_parent_edge(a, None, Placement::End).unwrap();
        assert_eq!(store.list_child_edges(None).unwrap(), vec![a]);
        assert_eq!(store.get_parent_edge(a).unwrap(), None);
        // Moving under a real parent drops the NULL edge; moving back restores one.
        let p = TaskId::new();
        store.set_parent_edge(a, Some(p), Placement::End).unwrap();
        assert_eq!(store.list_child_edges(None).unwrap(), []);
        store.set_parent_edge(a, None, Placement::End).unwrap();
        assert_eq!(store.list_child_edges(None).unwrap(), vec![a]);
    }

    #[test]
    fn set_parent_edge_should_reject_second_null_edge() {
        // The write API cannot express a second NULL edge (setting `None`
        // on a top-level task changes nothing); the raw duplicate is
        // rejected by SQLite in `bala-store`. Here: repeated top-level
        // writes never duplicate.
        let mut store = State::default();
        let a = TaskId::new();
        for _ in 0..3 {
            store.set_parent_edge(a, None, Placement::End).unwrap();
        }
        assert_eq!(store.list_child_edges(None).unwrap(), vec![a]);
    }

    #[test]
    fn list_child_edges_none_should_return_roots_in_position_order() {
        let mut store = State::default();
        let (a, b, c) = (TaskId::new(), TaskId::new(), TaskId::new());
        // Insertion order deliberately differs from any id ordering.
        for id in [b, c, a] {
            store.set_parent_edge(id, None, Placement::End).unwrap();
        }
        assert_eq!(store.list_child_edges(None).unwrap(), vec![b, c, a]);
    }

    #[test]
    fn set_parent_edge_then_list_child_edges_returns_child() {
        let store = InMemoryStore::default();
        let parent = TaskId::new();
        let child = TaskId::new();

        store
            .transaction(|tx| tx.set_parent_edge(child, Some(parent), Placement::End))
            .unwrap();

        let edges = store
            .transaction(|tx| tx.list_child_edges(Some(parent)))
            .unwrap();
        assert_eq!(edges, vec![child]);
    }
    #[test]
    fn get_task_should_not_return_a_soft_deleted_task() {
        let store = InMemoryStore::default();
        let mut task = sample_task("task", TaskStatus::Incomplete);
        task.deleted_at = Some(Utc::now());

        store.transaction(|tx| tx.put_task(&task)).unwrap();

        let fetched = store.transaction(|tx| tx.get_task(task.id)).unwrap();
        assert_eq!(fetched, None);
    }

    #[test]
    fn get_task_including_deleted_should_return_a_soft_deleted_task() {
        let store = InMemoryStore::default();
        let mut task = sample_task("task", TaskStatus::Incomplete);
        task.deleted_at = Some(Utc::now());

        store.transaction(|tx| tx.put_task(&task)).unwrap();

        let fetched = store
            .transaction(|tx| tx.get_task_including_deleted(task.id))
            .unwrap();
        assert_eq!(fetched, Some(task));
    }

    #[test]
    fn list_tasks_should_exclude_soft_deleted_by_default() {
        let store = InMemoryStore::default();
        let live = sample_task("task", TaskStatus::Incomplete);
        let mut deleted = sample_task("task", TaskStatus::Incomplete);
        deleted.deleted_at = Some(Utc::now());

        store
            .transaction(|tx| {
                tx.put_task(&live)?;
                tx.put_task(&deleted)?;
                Ok(())
            })
            .unwrap();

        let listed = store
            .transaction(|tx| tx.list_tasks(&TreeFilter::default()))
            .unwrap();
        assert_eq!(listed, vec![live]);
    }

    #[test]
    fn list_tasks_should_include_soft_deleted_when_filter_requests_it() {
        let store = InMemoryStore::default();
        let live = sample_task("task", TaskStatus::Incomplete);
        let mut deleted = sample_task("task", TaskStatus::Incomplete);
        deleted.deleted_at = Some(Utc::now());

        store
            .transaction(|tx| {
                tx.put_task(&live)?;
                tx.put_task(&deleted)?;
                Ok(())
            })
            .unwrap();

        let filter = TreeFilter {
            include_deleted: true,
            ..TreeFilter::default()
        };
        let mut listed = store.transaction(|tx| tx.list_tasks(&filter)).unwrap();
        listed.sort_by_key(|t| Uuid::from(t.id));
        let mut expected = vec![live, deleted];
        expected.sort_by_key(|t| Uuid::from(t.id));
        assert_eq!(listed, expected);
    }

    #[test]
    fn list_child_edges_for_task_with_no_children_returns_empty() {
        let store = InMemoryStore::default();
        let edges = store
            .transaction(|tx| tx.list_child_edges(Some(TaskId::new())))
            .unwrap();
        assert_eq!(edges, []);
    }

    #[test]
    fn put_task_type_then_get_task_types_returns_it() {
        let store = InMemoryStore::default();
        let task_type = TaskType {
            key: "goal".to_owned(),
            label: "Goal".to_owned(),
            color: None,
            sort_order: 0,
        };

        store
            .transaction(|tx| tx.put_task_type(&task_type))
            .unwrap();

        let listed = store.transaction(|tx| tx.get_task_types()).unwrap();
        assert_eq!(listed, vec![task_type]);
    }

    #[test]
    fn put_user_then_get_user_returns_same_user() {
        let store = InMemoryStore::default();
        let user = User {
            id: UserId::new(),
            name: "Ada".to_owned(),
        };

        store.transaction(|tx| tx.put_user(&user)).unwrap();

        let fetched = store.transaction(|tx| tx.get_user(user.id)).unwrap();
        assert_eq!(fetched, Some(user));
    }

    #[test]
    fn get_user_for_unknown_id_returns_none() {
        let store = InMemoryStore::default();
        let fetched = store.transaction(|tx| tx.get_user(UserId::new())).unwrap();
        assert_eq!(fetched, None);
    }

    #[test]
    fn list_users_returns_all_put_users() {
        let store = InMemoryStore::default();
        let a = User {
            id: UserId::new(),
            name: "Ada".to_owned(),
        };
        let b = User {
            id: UserId::new(),
            name: "Grace".to_owned(),
        };

        store
            .transaction(|tx| {
                tx.put_user(&a)?;
                tx.put_user(&b)?;
                Ok(())
            })
            .unwrap();

        let mut listed = store.transaction(|tx| tx.list_users()).unwrap();
        listed.sort_by_key(|u| Uuid::from(u.id));
        let mut expected = vec![a, b];
        expected.sort_by_key(|u| Uuid::from(u.id));
        assert_eq!(listed, expected);
    }

    fn ids(n: usize) -> Vec<TaskId> {
        (0..n).map(|_| TaskId::new()).collect()
    }

    fn append(tx: &mut dyn StoreTx, child: TaskId, parent: Option<TaskId>, at: Placement) {
        tx.set_parent_edge(child, parent, at).unwrap();
    }

    #[test]
    #[allow(clippy::many_single_char_names)]
    fn set_parent_edge_should_keep_position_when_parent_is_unchanged() {
        let store = InMemoryStore::default();
        let v = ids(4);
        let (p, y, child, z) = (v[0], v[1], v[2], v[3]);
        store
            .transaction(|tx| {
                append(tx, y, Some(p), Placement::End);
                append(tx, child, Some(p), Placement::End);
                append(tx, z, Some(p), Placement::End);
                append(tx, child, Some(p), Placement::After(z));
                append(tx, child, Some(p), Placement::End);
                Ok(())
            })
            .unwrap();

        let under_p = store
            .transaction(|tx| tx.list_child_edges(Some(p)))
            .unwrap();

        assert_eq!(under_p, [y, child, z]);
    }

    #[test]
    #[allow(clippy::many_single_char_names)]
    fn set_parent_edge_should_place_after_given_sibling() {
        let store = InMemoryStore::default();
        let v = ids(7);
        let (p, q, a, b, c, child, other) = (v[0], v[1], v[2], v[3], v[4], v[5], v[6]);
        store
            .transaction(|tx| {
                append(tx, a, Some(p), Placement::End);
                append(tx, b, Some(p), Placement::End);
                append(tx, c, Some(q), Placement::End);
                append(tx, child, Some(p), Placement::After(a));
                // Sibling not under the parent: falls back to End.
                append(tx, other, Some(p), Placement::After(c));
                Ok(())
            })
            .unwrap();

        let under_p = store
            .transaction(|tx| tx.list_child_edges(Some(p)))
            .unwrap();

        assert_eq!(under_p, [a, child, b, other]);
    }

    #[test]
    fn set_parent_edge_should_place_top_level_after_given_root() {
        let store = InMemoryStore::default();
        let v = ids(3);
        let (a, b, child) = (v[0], v[1], v[2]);
        store
            .transaction(|tx| {
                append(tx, a, None, Placement::End);
                append(tx, b, None, Placement::End);
                append(tx, child, None, Placement::After(a));
                Ok(())
            })
            .unwrap();

        let roots = store.transaction(|tx| tx.list_child_edges(None)).unwrap();

        assert_eq!(roots, [a, child, b]);
    }

    #[test]
    #[allow(clippy::many_single_char_names)]
    fn list_all_child_edges_should_return_every_parent_in_position_order() {
        let store = InMemoryStore::default();
        let v = ids(4);
        let (p, a, b, r) = (v[0], v[1], v[2], v[3]);
        store
            .transaction(|tx| {
                append(tx, p, None, Placement::End);
                append(tx, a, Some(p), Placement::End);
                append(tx, b, Some(p), Placement::End);
                append(tx, r, None, Placement::End);
                Ok(())
            })
            .unwrap();

        let all = store.transaction(|tx| tx.list_all_child_edges()).unwrap();
        let of = |parent| -> Vec<TaskId> {
            all.iter()
                .filter(|(p, _)| *p == parent)
                .map(|&(_, c)| c)
                .collect()
        };

        assert_eq!(of(None), [p, r]);
        assert_eq!(of(Some(p)), [a, b]);
    }

    #[test]
    #[allow(clippy::many_single_char_names)]
    fn swap_child_positions_should_swap_the_two_children() {
        let store = InMemoryStore::default();
        let v = ids(4);
        let (p, a, b, c) = (v[0], v[1], v[2], v[3]);
        store
            .transaction(|tx| {
                append(tx, a, Some(p), Placement::End);
                append(tx, b, Some(p), Placement::End);
                append(tx, c, Some(p), Placement::End);
                tx.swap_child_positions(Some(p), a, c)
            })
            .unwrap();

        let under_p = store
            .transaction(|tx| tx.list_child_edges(Some(p)))
            .unwrap();

        assert_eq!(under_p, [c, b, a]);
    }

    #[test]
    fn swap_child_positions_should_ignore_task_not_under_parent() {
        let store = InMemoryStore::default();
        let v = ids(5);
        let (p, a, b) = (v[0], v[1], v[2]);
        store
            .transaction(|tx| {
                append(tx, a, Some(p), Placement::End);
                tx.swap_child_positions(Some(p), a, b)
            })
            .unwrap();

        let under_p = store
            .transaction(|tx| tx.list_child_edges(Some(p)))
            .unwrap();

        assert_eq!(under_p, [a]);
    }

    fn dep(predecessor_id: TaskId, dep_type: DependencyType) -> Dependency {
        Dependency {
            predecessor_id,
            dep_type,
        }
    }

    #[test]
    fn add_dependency_edge_then_list_dependency_edges_returns_it() {
        let store = InMemoryStore::default();
        let v = ids(2);
        let (pred, succ) = (v[0], v[1]);

        store
            .transaction(|tx| tx.add_dependency_edge(pred, succ, DependencyType::StartToStart))
            .unwrap();

        let edges = store
            .transaction(|tx| tx.list_dependency_edges(succ))
            .unwrap();
        assert_eq!(edges, [dep(pred, DependencyType::StartToStart)]);
        // The edge is directed: the predecessor does not depend on anything.
        let reverse = store
            .transaction(|tx| tx.list_dependency_edges(pred))
            .unwrap();
        assert_eq!(reverse, []);
    }

    #[test]
    fn add_dependency_edge_should_replace_type_on_existing_pair() {
        let store = InMemoryStore::default();
        let v = ids(4);
        let (a, b, succ, other) = (v[0], v[1], v[2], v[3]);
        store
            .transaction(|tx| {
                tx.add_dependency_edge(a, succ, DependencyType::FinishToStart)?;
                tx.add_dependency_edge(b, succ, DependencyType::FinishToStart)?;
                tx.add_dependency_edge(a, other, DependencyType::FinishToStart)?;
                tx.add_dependency_edge(a, succ, DependencyType::FinishToFinish)
            })
            .unwrap();

        let edges = store
            .transaction(|tx| tx.list_dependency_edges(succ))
            .unwrap();
        let successors = store.transaction(|tx| tx.list_successor_edges(a)).unwrap();

        // One edge per pair, and the replaced edge keeps its place in both lists.
        assert_eq!(
            edges,
            [
                dep(a, DependencyType::FinishToFinish),
                dep(b, DependencyType::FinishToStart)
            ]
        );
        assert_eq!(successors, [succ, other]);
    }

    #[test]
    fn list_dependency_edges_should_return_predecessors_in_insertion_order() {
        let store = InMemoryStore::default();
        let v = ids(4);
        let (a, b, c, succ) = (v[0], v[1], v[2], v[3]);
        // Insertion order deliberately differs from the order the ids were made in.
        store
            .transaction(|tx| {
                tx.add_dependency_edge(c, succ, DependencyType::StartToFinish)?;
                tx.add_dependency_edge(a, succ, DependencyType::FinishToStart)?;
                tx.add_dependency_edge(b, succ, DependencyType::StartToStart)
            })
            .unwrap();

        let edges = store
            .transaction(|tx| tx.list_dependency_edges(succ))
            .unwrap();

        assert_eq!(
            edges,
            [
                dep(c, DependencyType::StartToFinish),
                dep(a, DependencyType::FinishToStart),
                dep(b, DependencyType::StartToStart)
            ]
        );
    }

    #[test]
    fn list_successor_edges_should_return_all_successors_of_multi_successor_predecessor() {
        let store = InMemoryStore::default();
        let v = ids(5);
        let (pred, a, b, c, unrelated) = (v[0], v[1], v[2], v[3], v[4]);
        store
            .transaction(|tx| {
                tx.add_dependency_edge(pred, c, DependencyType::FinishToStart)?;
                tx.add_dependency_edge(unrelated, a, DependencyType::FinishToStart)?;
                tx.add_dependency_edge(pred, a, DependencyType::StartToStart)?;
                tx.add_dependency_edge(pred, b, DependencyType::FinishToFinish)
            })
            .unwrap();

        let successors = store
            .transaction(|tx| tx.list_successor_edges(pred))
            .unwrap();
        let of_leaf = store.transaction(|tx| tx.list_successor_edges(c)).unwrap();

        assert_eq!(successors, [c, a, b]);
        assert_eq!(of_leaf, []);
    }

    #[test]
    fn remove_dependency_edge_should_remove_only_that_pair() {
        let store = InMemoryStore::default();
        let v = ids(4);
        let (a, b, succ, other) = (v[0], v[1], v[2], v[3]);
        store
            .transaction(|tx| {
                tx.add_dependency_edge(a, succ, DependencyType::FinishToStart)?;
                tx.add_dependency_edge(b, succ, DependencyType::StartToStart)?;
                tx.add_dependency_edge(a, other, DependencyType::FinishToFinish)?;
                tx.remove_dependency_edge(a, succ)
            })
            .unwrap();

        let of_succ = store
            .transaction(|tx| tx.list_dependency_edges(succ))
            .unwrap();
        let of_other = store
            .transaction(|tx| tx.list_dependency_edges(other))
            .unwrap();
        let after_a = store.transaction(|tx| tx.list_successor_edges(a)).unwrap();
        let after_b = store.transaction(|tx| tx.list_successor_edges(b)).unwrap();

        assert_eq!(of_succ, [dep(b, DependencyType::StartToStart)]);
        assert_eq!(of_other, [dep(a, DependencyType::FinishToFinish)]);
        assert_eq!(after_a, [other]);
        assert_eq!(after_b, [succ]);
    }

    #[test]
    fn remove_dependency_edge_should_do_nothing_for_a_missing_pair() {
        let store = InMemoryStore::default();
        let v = ids(3);
        let (pred, succ, stranger) = (v[0], v[1], v[2]);
        store
            .transaction(|tx| {
                tx.add_dependency_edge(pred, succ, DependencyType::FinishToStart)?;
                // Never linked, and the existing pair the wrong way round.
                tx.remove_dependency_edge(stranger, succ)?;
                tx.remove_dependency_edge(succ, pred)
            })
            .unwrap();

        let edges = store
            .transaction(|tx| tx.list_dependency_edges(succ))
            .unwrap();
        let successors = store
            .transaction(|tx| tx.list_successor_edges(pred))
            .unwrap();

        assert_eq!(edges, [dep(pred, DependencyType::FinishToStart)]);
        assert_eq!(successors, [succ]);
    }

    #[test]
    fn get_task_should_fill_depends_on_from_dependency_edges() {
        let store = InMemoryStore::default();
        let a = sample_task("task", TaskStatus::Incomplete);
        let b = sample_task("task", TaskStatus::Incomplete);
        let succ = sample_task("task", TaskStatus::Incomplete);
        store
            .transaction(|tx| {
                tx.put_task(&a)?;
                tx.put_task(&b)?;
                tx.put_task(&succ)?;
                tx.add_dependency_edge(a.id, succ.id, DependencyType::FinishToStart)?;
                tx.add_dependency_edge(b.id, succ.id, DependencyType::StartToStart)
            })
            .unwrap();

        let fetched = store.transaction(|tx| tx.get_task(succ.id)).unwrap();
        let predecessor = store.transaction(|tx| tx.get_task(a.id)).unwrap();

        assert_eq!(
            fetched.unwrap().depends_on,
            [
                dep(a.id, DependencyType::FinishToStart),
                dep(b.id, DependencyType::StartToStart)
            ]
        );
        // The edge is directed: a predecessor does not depend on its successor.
        assert_eq!(predecessor.unwrap().depends_on, []);
    }

    #[test]
    fn get_task_including_deleted_should_fill_depends_on() {
        let store = InMemoryStore::default();
        let mut pred = sample_task("task", TaskStatus::Incomplete);
        pred.deleted_at = Some(Utc::now());
        let mut deleted = sample_task("task", TaskStatus::Incomplete);
        deleted.deleted_at = Some(Utc::now());
        let live = sample_task("task", TaskStatus::Incomplete);
        store
            .transaction(|tx| {
                tx.put_task(&pred)?;
                tx.put_task(&deleted)?;
                tx.put_task(&live)?;
                tx.add_dependency_edge(pred.id, deleted.id, DependencyType::FinishToFinish)?;
                tx.add_dependency_edge(deleted.id, live.id, DependencyType::FinishToStart)
            })
            .unwrap();

        let of_deleted = store
            .transaction(|tx| tx.get_task_including_deleted(deleted.id))
            .unwrap();
        let of_live = store
            .transaction(|tx| tx.get_task_including_deleted(live.id))
            .unwrap();

        // A soft-deleted task keeps its dependency edges, at either end.
        assert_eq!(
            of_deleted.unwrap().depends_on,
            [dep(pred.id, DependencyType::FinishToFinish)]
        );
        assert_eq!(
            of_live.unwrap().depends_on,
            [dep(deleted.id, DependencyType::FinishToStart)]
        );
    }

    #[test]
    fn list_tasks_should_fill_depends_on_for_each_task() {
        let store = InMemoryStore::default();
        let a = sample_task("task", TaskStatus::Incomplete);
        let b = sample_task("task", TaskStatus::Incomplete);
        let c = sample_task("task", TaskStatus::Incomplete);
        store
            .transaction(|tx| {
                tx.put_task(&a)?;
                tx.put_task(&b)?;
                tx.put_task(&c)?;
                tx.add_dependency_edge(a.id, b.id, DependencyType::StartToFinish)?;
                tx.add_dependency_edge(b.id, c.id, DependencyType::FinishToStart)?;
                tx.add_dependency_edge(a.id, c.id, DependencyType::StartToStart)
            })
            .unwrap();

        let listed = store
            .transaction(|tx| tx.list_tasks(&TreeFilter::default()))
            .unwrap();

        let depends_on = |id: TaskId| {
            let task = listed.iter().find(|task| task.id == id).unwrap();
            task.depends_on.clone()
        };
        assert_eq!(listed.len(), 3);
        assert_eq!(depends_on(a.id), []);
        assert_eq!(depends_on(b.id), [dep(a.id, DependencyType::StartToFinish)]);
        assert_eq!(
            depends_on(c.id),
            [
                dep(b.id, DependencyType::FinishToStart),
                dep(a.id, DependencyType::StartToStart)
            ]
        );
    }

    #[test]
    fn put_task_should_not_change_dependency_edges() {
        let store = InMemoryStore::default();
        let pred = sample_task("task", TaskStatus::Incomplete);
        let other = sample_task("task", TaskStatus::Incomplete);
        let mut succ = sample_task("task", TaskStatus::Incomplete);
        store
            .transaction(|tx| {
                tx.put_task(&pred)?;
                tx.put_task(&other)?;
                tx.put_task(&succ)?;
                tx.add_dependency_edge(pred.id, succ.id, DependencyType::FinishToStart)
            })
            .unwrap();

        // Names a predecessor the edges don't have and drops the one they do.
        succ.depends_on = vec![dep(other.id, DependencyType::StartToStart)];
        store.transaction(|tx| tx.put_task(&succ)).unwrap();

        let edges = store
            .transaction(|tx| tx.list_dependency_edges(succ.id))
            .unwrap();
        let of_other = store
            .transaction(|tx| tx.list_successor_edges(other.id))
            .unwrap();
        let fetched = store.transaction(|tx| tx.get_task(succ.id)).unwrap();

        assert_eq!(edges, [dep(pred.id, DependencyType::FinishToStart)]);
        assert_eq!(of_other, []);
        assert_eq!(
            fetched.unwrap().depends_on,
            [dep(pred.id, DependencyType::FinishToStart)]
        );
    }
}
