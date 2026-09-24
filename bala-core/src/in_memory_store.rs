//! An in-memory [`Store`]/[`StoreTx`] implementation for tests.
//!
//! Not a production backend — no persistence, no concurrency control
//! beyond a single [`RefCell`]. It exists so `bala-core`'s algorithms (and
//! this checkpoint's own put/get/list/edge round-trip tests) can run
//! without a real database.
//!
//! Parent edges are kept in two maps mirroring the SQLite `parent_edges`
//! table: child -> parents (`None` = the NULL top-level edge) and parent ->
//! children in position order (`None` = the root list). A child's position
//! is its index in the parent's list.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::model::{Parents, Placement, Task, TaskId, TaskType, TreeFilter, User, UserId};
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
    /// child -> its parents; `None` is a top-level task's NULL edge.
    parents_of: HashMap<TaskId, Vec<Option<TaskId>>>,
    /// parent (`None` = the root list) -> children, in position order.
    children_of: HashMap<Option<TaskId>, Vec<TaskId>>,
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
            .cloned())
    }

    fn put_task(&mut self, task: &Task) -> Result<(), StoreError> {
        // `progress` is library-computed, never persisted (`bala-store`'s
        // SQLite backend has no column for it and always reconstructs
        // `0.0` on read) — reset it here too, so a caller driving
        // `StoreTx` directly sees the same non-persistence behavior
        // regardless of which `Store` backend is live. `Core` always
        // recomputes the real value before returning a `Task` to its own
        // callers, so this only matters to a caller bypassing `Core`.
        let mut task = task.clone();
        task.progress = 0.0;
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
            .cloned()
            .collect())
    }

    fn list_parent_edges(&mut self, id: TaskId) -> Result<Vec<TaskId>, StoreError> {
        Ok(self
            .parents_of
            .get(&id)
            .map(|ps| ps.iter().copied().flatten().collect())
            .unwrap_or_default())
    }

    fn replace_parent_edges(
        &mut self,
        child: TaskId,
        parents: &Parents,
        placement: Placement,
    ) -> Result<(), StoreError> {
        // Desired parents: `None` = the NULL edge (top level).
        let mut wanted: Vec<Option<TaskId>> = Vec::new();
        match parents {
            Parents::TopLevel => wanted.push(None),
            Parents::Under(ids) => {
                for &id in ids {
                    if !wanted.contains(&Some(id)) {
                        wanted.push(Some(id));
                    }
                }
            }
        }
        let existing = self.parents_of.remove(&child).unwrap_or_default();
        let mut kept = Vec::with_capacity(wanted.len());
        for old in existing {
            if wanted.contains(&old) {
                kept.push(old); // a kept edge keeps its position
            } else if let Some(siblings) = self.children_of.get_mut(&old) {
                siblings.retain(|&c| c != child);
            }
        }
        for parent in wanted {
            if !kept.contains(&parent) {
                let siblings = self.children_of.entry(parent).or_default();
                let at = match placement {
                    Placement::After(sibling) if sibling != child => siblings
                        .iter()
                        .position(|&c| c == sibling)
                        .map_or(siblings.len(), |i| i + 1),
                    _ => siblings.len(),
                };
                siblings.insert(at, child);
                kept.push(parent);
            }
        }
        self.parents_of.insert(child, kept);
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

    fn get_task_including_deleted(&mut self, id: TaskId) -> Result<Option<Task>, StoreError> {
        Ok(self.tasks.get(&id).cloned())
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
            parent_ids: Vec::new(),
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
            assignee_id: None,
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
    fn list_parent_edges_for_task_with_no_parents_returns_empty() {
        let store = InMemoryStore::default();
        let edges = store
            .transaction(|tx| tx.list_parent_edges(TaskId::new()))
            .unwrap();
        assert!(edges.is_empty());
    }

    #[test]
    fn replace_parent_edges_then_list_parent_edges_returns_it() {
        let store = InMemoryStore::default();
        let parent = TaskId::new();
        let child = TaskId::new();

        store
            .transaction(|tx| {
                tx.replace_parent_edges(child, &Parents::Under(vec![parent]), Placement::End)
            })
            .unwrap();

        let edges = store.transaction(|tx| tx.list_parent_edges(child)).unwrap();
        assert_eq!(edges, vec![parent]);
    }

    #[test]
    fn top_level_task_should_have_single_null_parent_edge() {
        let mut store = State::default();
        let a = TaskId::new();
        store
            .replace_parent_edges(a, &Parents::TopLevel, Placement::End)
            .unwrap();
        store
            .replace_parent_edges(a, &Parents::TopLevel, Placement::End)
            .unwrap();
        assert_eq!(store.list_child_edges(None).unwrap(), vec![a]);
        assert!(store.list_parent_edges(a).unwrap().is_empty());
        // Moving under a real parent drops the NULL edge; moving back restores one.
        let p = TaskId::new();
        store
            .replace_parent_edges(a, &Parents::Under(vec![p]), Placement::End)
            .unwrap();
        assert!(store.list_child_edges(None).unwrap().is_empty());
        store
            .replace_parent_edges(a, &Parents::TopLevel, Placement::End)
            .unwrap();
        assert_eq!(store.list_child_edges(None).unwrap(), vec![a]);
    }

    #[test]
    fn replace_parent_edges_should_reject_second_null_edge() {
        // The write API cannot express a second NULL edge (`Parents::TopLevel`
        // is idempotent); the raw duplicate is rejected by SQLite in
        // `bala-store`. Here: repeated TopLevel writes never duplicate.
        let mut store = State::default();
        let a = TaskId::new();
        for _ in 0..3 {
            store
                .replace_parent_edges(a, &Parents::TopLevel, Placement::End)
                .unwrap();
        }
        assert_eq!(store.list_child_edges(None).unwrap(), vec![a]);
    }

    #[test]
    fn list_child_edges_none_should_return_roots_in_position_order() {
        let mut store = State::default();
        let (a, b, c) = (TaskId::new(), TaskId::new(), TaskId::new());
        // Insertion order deliberately differs from any id ordering.
        for id in [b, c, a] {
            store
                .replace_parent_edges(id, &Parents::TopLevel, Placement::End)
                .unwrap();
        }
        assert_eq!(store.list_child_edges(None).unwrap(), vec![b, c, a]);
    }

    #[test]
    fn replace_parent_edges_should_replace_whole_set_atomically() {
        let store = InMemoryStore::default();
        let (a, b, c, child) = (TaskId::new(), TaskId::new(), TaskId::new(), TaskId::new());
        store
            .transaction(|tx| {
                tx.replace_parent_edges(child, &Parents::Under(vec![a, b]), Placement::End)
            })
            .unwrap();

        store
            .transaction(|tx| {
                tx.replace_parent_edges(child, &Parents::Under(vec![c]), Placement::End)
            })
            .unwrap();

        let edges = store.transaction(|tx| tx.list_parent_edges(child)).unwrap();
        assert_eq!(edges, vec![c]);
        assert!(
            store
                .transaction(|tx| tx.list_child_edges(Some(a)))
                .unwrap()
                .is_empty()
        );

        store
            .transaction(|tx| tx.replace_parent_edges(child, &Parents::TopLevel, Placement::End))
            .unwrap();
        let edges = store.transaction(|tx| tx.list_parent_edges(child)).unwrap();
        assert!(edges.is_empty());
    }

    #[test]
    fn replace_parent_edges_should_keep_edges_it_was_asked_to_keep() {
        let store = InMemoryStore::default();
        let (a, b, c, child) = (TaskId::new(), TaskId::new(), TaskId::new(), TaskId::new());
        store
            .transaction(|tx| {
                tx.replace_parent_edges(child, &Parents::Under(vec![a, b]), Placement::End)
            })
            .unwrap();

        store
            .transaction(|tx| {
                tx.replace_parent_edges(child, &Parents::Under(vec![b, c, b]), Placement::End)
            })
            .unwrap();

        let mut edges = store.transaction(|tx| tx.list_parent_edges(child)).unwrap();
        edges.sort_by_key(|&id| Uuid::from(id));
        let mut expected = vec![b, c];
        expected.sort_by_key(|&id| Uuid::from(id));
        assert_eq!(edges, expected);
        assert_eq!(
            store
                .transaction(|tx| tx.list_child_edges(Some(b)))
                .unwrap(),
            vec![child]
        );
    }

    #[test]
    fn replace_parent_edges_then_list_child_edges_returns_child() {
        let store = InMemoryStore::default();
        let parent = TaskId::new();
        let child = TaskId::new();

        store
            .transaction(|tx| {
                tx.replace_parent_edges(child, &Parents::Under(vec![parent]), Placement::End)
            })
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
        assert!(edges.is_empty());
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

    fn append(tx: &mut dyn StoreTx, child: TaskId, parents: &Parents, at: Placement) {
        tx.replace_parent_edges(child, parents, at).unwrap();
    }

    #[test]
    #[allow(clippy::many_single_char_names)]
    fn replace_parent_edges_should_keep_position_of_retained_parents() {
        let store = InMemoryStore::default();
        let v = ids(5);
        let (p, q, y, child, z) = (v[0], v[1], v[2], v[3], v[4]);
        store
            .transaction(|tx| {
                append(tx, y, &Parents::Under(vec![p]), Placement::End);
                append(tx, child, &Parents::Under(vec![p]), Placement::End);
                append(tx, z, &Parents::Under(vec![p]), Placement::End);
                append(tx, child, &Parents::Under(vec![p, q]), Placement::After(z));
                Ok(())
            })
            .unwrap();

        let under_p = store
            .transaction(|tx| tx.list_child_edges(Some(p)))
            .unwrap();
        let under_q = store
            .transaction(|tx| tx.list_child_edges(Some(q)))
            .unwrap();

        assert_eq!(under_p, [y, child, z]);
        assert_eq!(under_q, [child]);
    }

    #[test]
    #[allow(clippy::many_single_char_names)]
    fn replace_parent_edges_should_place_after_given_sibling() {
        let store = InMemoryStore::default();
        let v = ids(7);
        let (p, q, a, b, c, child, other) = (v[0], v[1], v[2], v[3], v[4], v[5], v[6]);
        store
            .transaction(|tx| {
                append(tx, a, &Parents::Under(vec![p]), Placement::End);
                append(tx, b, &Parents::Under(vec![p]), Placement::End);
                append(tx, c, &Parents::Under(vec![q]), Placement::End);
                append(tx, child, &Parents::Under(vec![p, q]), Placement::After(a));
                // Sibling not under the parent: falls back to End.
                append(tx, other, &Parents::Under(vec![p]), Placement::After(c));
                Ok(())
            })
            .unwrap();

        let under_p = store
            .transaction(|tx| tx.list_child_edges(Some(p)))
            .unwrap();
        let under_q = store
            .transaction(|tx| tx.list_child_edges(Some(q)))
            .unwrap();

        assert_eq!(under_p, [a, child, b, other]);
        assert_eq!(under_q, [c, child]);
    }

    #[test]
    fn replace_parent_edges_should_place_top_level_after_given_root() {
        let store = InMemoryStore::default();
        let v = ids(3);
        let (a, b, child) = (v[0], v[1], v[2]);
        store
            .transaction(|tx| {
                append(tx, a, &Parents::TopLevel, Placement::End);
                append(tx, b, &Parents::TopLevel, Placement::End);
                append(tx, child, &Parents::TopLevel, Placement::After(a));
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
                append(tx, p, &Parents::TopLevel, Placement::End);
                append(tx, a, &Parents::Under(vec![p]), Placement::End);
                append(tx, b, &Parents::Under(vec![p]), Placement::End);
                append(tx, r, &Parents::TopLevel, Placement::End);
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
    fn swap_child_positions_should_swap_only_the_given_parent() {
        let store = InMemoryStore::default();
        let v = ids(5);
        let (p, q, a, b, c) = (v[0], v[1], v[2], v[3], v[4]);
        store
            .transaction(|tx| {
                append(tx, a, &Parents::Under(vec![p, q]), Placement::End);
                append(tx, b, &Parents::Under(vec![p, q]), Placement::End);
                append(tx, c, &Parents::Under(vec![p]), Placement::End);
                tx.swap_child_positions(Some(p), a, c)
            })
            .unwrap();

        let under_p = store
            .transaction(|tx| tx.list_child_edges(Some(p)))
            .unwrap();
        let under_q = store
            .transaction(|tx| tx.list_child_edges(Some(q)))
            .unwrap();

        assert_eq!(under_p, [c, b, a]);
        assert_eq!(under_q, [a, b]);
    }

    #[test]
    fn swap_child_positions_should_ignore_task_not_under_parent() {
        let store = InMemoryStore::default();
        let v = ids(5);
        let (p, a, b) = (v[0], v[1], v[2]);
        store
            .transaction(|tx| {
                append(tx, a, &Parents::Under(vec![p]), Placement::End);
                tx.swap_child_positions(Some(p), a, b)
            })
            .unwrap();

        let under_p = store
            .transaction(|tx| tx.list_child_edges(Some(p)))
            .unwrap();

        assert_eq!(under_p, [a]);
    }
}
