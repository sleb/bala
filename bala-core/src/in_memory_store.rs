//! An in-memory [`Store`]/[`StoreTx`] implementation for tests.
//!
//! Not a production backend — no persistence, no concurrency control
//! beyond a single [`RefCell`]. It exists so `bala-core`'s algorithms (and
//! this checkpoint's own put/get/list/edge round-trip tests) can run
//! without a real database.
//!
//! `TreeFilter::include_deleted` is currently a no-op here: [`Task`] has no
//! `deleted_at` field yet at this checkpoint's scope (soft-delete lands in
//! a later story), so there is nothing for it to filter.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::model::{Task, TaskId, TaskType, TreeFilter};
use crate::store::{Store, StoreError, StoreTx};

/// Test double for [`Store`], backed by in-memory maps.
#[derive(Debug, Default)]
pub struct InMemoryStore {
    inner: RefCell<State>,
}

#[derive(Debug, Default)]
struct State {
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
    fn get_task(&mut self, id: TaskId) -> Result<Option<Task>, StoreError> {
        Ok(self.tasks.get(&id).cloned())
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
            created_at: now,
            updated_at: now,
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
}
