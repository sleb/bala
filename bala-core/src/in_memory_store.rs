//! An in-memory [`Store`]/[`StoreTx`] implementation for tests.
//!
//! Not a production backend — no persistence, no concurrency control
//! beyond a single [`RefCell`]. It exists so `bala-core`'s algorithms (and
//! this checkpoint's own put/get/list/edge round-trip tests) can run
//! without a real database.
//!
//! `parent_edges` is keyed child -> its parents, so `list_child_edges`
//! (the reverse direction) scans every entry rather than maintaining a
//! second map — fine for an in-memory fake that isn't optimized for scale.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::model::{Task, TaskId, TaskType, TreeFilter, User, UserId};
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
    /// child -> its parents.
    parent_edges: HashMap<TaskId, Vec<TaskId>>,
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
        self.tasks.insert(task.id, task.clone());
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
        Ok(self.parent_edges.get(&id).cloned().unwrap_or_default())
    }

    fn add_parent_edge(&mut self, parent: TaskId, child: TaskId) -> Result<(), StoreError> {
        self.parent_edges.entry(child).or_default().push(parent);
        Ok(())
    }

    fn list_child_edges(&mut self, id: TaskId) -> Result<Vec<TaskId>, StoreError> {
        Ok(self
            .parent_edges
            .iter()
            .filter(|(_, parents)| parents.contains(&id))
            .map(|(&child, _)| child)
            .collect())
    }

    fn remove_parent_edge(&mut self, parent: TaskId, child: TaskId) -> Result<(), StoreError> {
        if let Some(parents) = self.parent_edges.get_mut(&child) {
            parents.retain(|&p| p != parent);
        }
        Ok(())
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
            start_date: None,
            due_date: None,
            assignee_id: None,
            created_at: now,
            updated_at: now,
            deleted_at: None,
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
    fn add_parent_edge_then_list_parent_edges_returns_it() {
        let store = InMemoryStore::default();
        let parent = TaskId::new();
        let child = TaskId::new();

        store
            .transaction(|tx| tx.add_parent_edge(parent, child))
            .unwrap();

        let edges = store.transaction(|tx| tx.list_parent_edges(child)).unwrap();
        assert_eq!(edges, vec![parent]);
    }

    #[test]
    fn add_parent_edge_supports_multiple_parents() {
        let store = InMemoryStore::default();
        let parent_a = TaskId::new();
        let parent_b = TaskId::new();
        let child = TaskId::new();

        store
            .transaction(|tx| {
                tx.add_parent_edge(parent_a, child)?;
                tx.add_parent_edge(parent_b, child)?;
                Ok(())
            })
            .unwrap();

        let mut edges = store.transaction(|tx| tx.list_parent_edges(child)).unwrap();
        edges.sort_by_key(|&id| Uuid::from(id));
        let mut expected = vec![parent_a, parent_b];
        expected.sort_by_key(|&id| Uuid::from(id));
        assert_eq!(edges, expected);
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
            .transaction(|tx| tx.list_child_edges(TaskId::new()))
            .unwrap();
        assert!(edges.is_empty());
    }

    #[test]
    fn add_parent_edge_then_list_child_edges_returns_child() {
        let store = InMemoryStore::default();
        let parent = TaskId::new();
        let child = TaskId::new();

        store
            .transaction(|tx| tx.add_parent_edge(parent, child))
            .unwrap();

        let edges = store.transaction(|tx| tx.list_child_edges(parent)).unwrap();
        assert_eq!(edges, vec![child]);
    }

    #[test]
    fn remove_parent_edge_removes_only_that_edge() {
        let store = InMemoryStore::default();
        let parent_a = TaskId::new();
        let parent_b = TaskId::new();
        let child = TaskId::new();

        store
            .transaction(|tx| {
                tx.add_parent_edge(parent_a, child)?;
                tx.add_parent_edge(parent_b, child)?;
                Ok(())
            })
            .unwrap();

        store
            .transaction(|tx| tx.remove_parent_edge(parent_a, child))
            .unwrap();

        let edges = store.transaction(|tx| tx.list_parent_edges(child)).unwrap();
        assert_eq!(edges, vec![parent_b]);
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
}
