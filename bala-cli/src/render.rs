//! Pure task-row projection for the TUI's task list view.
//!
//! `task_rows` turns already-fetched `Task`s (plus small lookup maps for
//! type labels and user names) into `TaskRow`s ready for display. It does no
//! I/O and never touches `Core` itself — later checkpoints (the TUI's `app`
//! and `screens` modules) call it after fetching data through `Core`, which
//! keeps this projection unit-testable without a terminal or a database.

use std::collections::HashMap;

use bala_core::{Task, TaskId, TaskStatus, UserId};

/// One row of the task list view: a top-level task's display-ready fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRow {
    pub id: TaskId,
    pub title: String,
    pub type_label: String,
    pub status: TaskStatus,
    pub assignee_name: Option<String>,
}

/// Projects top-level tasks into display-ready `TaskRow`s, in the order
/// given.
///
/// Tasks with a non-empty `parent_ids` (i.e. subtasks) are excluded; full
/// tree layout is a later concern. `type_labels` maps `Task::type_key` to
/// its display label; a `type_key` missing from the map falls back to the
/// raw key rather than panicking. `user_names` maps `UserId` to display
/// name; an unassigned task, or one assigned to an id missing from the map,
/// gets `assignee_name: None`.
#[must_use]
pub fn task_rows(
    tasks: &[Task],
    type_labels: &HashMap<String, String>,
    user_names: &HashMap<UserId, String>,
) -> Vec<TaskRow> {
    tasks
        .iter()
        .filter(|task| task.parent_ids.is_empty())
        .map(|task| TaskRow {
            id: task.id,
            title: task.title.clone(),
            type_label: type_labels
                .get(&task.type_key)
                .cloned()
                .unwrap_or_else(|| task.type_key.clone()),
            status: task.status,
            assignee_name: task.assignee_id.and_then(|id| user_names.get(&id).cloned()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    /// Minimal `Task` fixture: fills required fields with dummy values so
    /// each test only needs to override what it cares about.
    fn task(id: TaskId, title: &str, parent_ids: Vec<TaskId>) -> Task {
        let now = Utc::now();
        Task {
            id,
            title: title.to_string(),
            description: None,
            parent_ids,
            type_key: "task".to_string(),
            status: TaskStatus::Incomplete,
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
    fn task_rows_should_include_only_top_level_tasks() {
        let top_level = task(TaskId::new(), "Top level", vec![]);
        let child = task(TaskId::new(), "Child", vec![top_level.id]);
        let tasks = vec![top_level.clone(), child];

        let rows = task_rows(&tasks, &HashMap::new(), &HashMap::new());

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, top_level.id);
        assert_eq!(rows[0].title, "Top level");
    }

    #[test]
    fn task_rows_should_include_title_type_label_and_status() {
        let mut input = task(TaskId::new(), "Ship the thing", vec![]);
        input.type_key = "initiative".to_string();
        input.status = TaskStatus::Complete;
        let type_labels: HashMap<String, String> =
            [("initiative".to_string(), "Initiative".to_string())]
                .into_iter()
                .collect();

        let rows = task_rows(&[input], &type_labels, &HashMap::new());

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Ship the thing");
        assert_eq!(rows[0].type_label, "Initiative");
        assert_eq!(rows[0].status, TaskStatus::Complete);
    }

    #[test]
    fn task_rows_should_show_assignee_name_when_assigned() {
        let user_id = UserId::new();
        let mut input = task(TaskId::new(), "Assigned task", vec![]);
        input.assignee_id = Some(user_id);
        let user_names: HashMap<UserId, String> = [(user_id, "Ada Lovelace".to_string())]
            .into_iter()
            .collect();

        let rows = task_rows(&[input], &HashMap::new(), &user_names);

        assert_eq!(rows[0].assignee_name, Some("Ada Lovelace".to_string()));
    }

    #[test]
    fn task_rows_should_show_unassigned_when_no_assignee() {
        let input = task(TaskId::new(), "Unassigned task", vec![]);

        let rows = task_rows(&[input], &HashMap::new(), &HashMap::new());

        assert_eq!(rows[0].assignee_name, None);
    }

    #[test]
    fn task_rows_should_return_empty_vec_when_no_top_level_tasks() {
        let parent_id = TaskId::new();
        let child_a = task(TaskId::new(), "Child A", vec![parent_id]);
        let child_b = task(TaskId::new(), "Child B", vec![parent_id]);

        let rows = task_rows(&[child_a, child_b], &HashMap::new(), &HashMap::new());

        assert!(rows.is_empty());
    }
}
