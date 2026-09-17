//! Pure task-row projection for the TUI's task list view.
//!
//! `task_rows` turns already-fetched `Task`s (plus small lookup maps for
//! type labels and user names) into `TaskRow`s ready for display. It does no
//! I/O and never touches `Core` itself — later checkpoints (the TUI's `app`
//! and `screens` modules) call it after fetching data through `Core`, which
//! keeps this projection unit-testable without a terminal or a database.

use std::collections::{HashMap, HashSet};

use bala_core::{Task, TaskId, TaskStatus, UserId};

/// One row of the task list view: a task's display-ready fields, plus its
/// nesting depth under whichever root it's being rendered under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRow {
    pub id: TaskId,
    pub title: String,
    pub type_label: String,
    pub status: TaskStatus,
    pub assignee_name: Option<String>,
    pub depth: usize,
}

/// Projects every task in `tasks` into a display-ready `TaskRow`, nested
/// under its parent(s).
///
/// Rows are produced by a pre-order depth-first walk starting from every
/// "root" task, where a root is a task with no `parent_ids`, or whose
/// listed parents are all absent from `tasks` (defensive against a partial
/// input — see below). Roots are visited in `tasks`' input order, and each
/// task's children (from its `parent_ids`) are visited in `tasks`' input
/// order too. A task with more than one parent present in `tasks` is
/// visited — and so emitted as a row — once per present parent, each time
/// at the depth appropriate to that parent's subtree; the tree is always
/// fully expanded (no collapse/expand yet).
///
/// `tasks` is expected to be a full `Core::get_tree()` result, which always
/// includes every ancestor of any task it contains. A partial slice missing
/// some parent is still handled without panicking or silently dropping the
/// orphaned child: that child renders as a root at `depth: 0` instead.
///
/// `type_labels` maps `Task::type_key` to its display label; a `type_key`
/// missing from the map falls back to the raw key rather than panicking.
/// `user_names` maps `UserId` to display name; an unassigned task, or one
/// assigned to an id missing from the map, gets `assignee_name: None`.
#[must_use]
pub fn task_rows(
    tasks: &[Task],
    type_labels: &HashMap<String, String>,
    user_names: &HashMap<UserId, String>,
) -> Vec<TaskRow> {
    let known_ids: HashSet<TaskId> = tasks.iter().map(|task| task.id).collect();

    let mut children_by_parent: HashMap<TaskId, Vec<&Task>> = HashMap::new();
    for task in tasks {
        for parent_id in &task.parent_ids {
            children_by_parent.entry(*parent_id).or_default().push(task);
        }
    }

    let is_root = |task: &Task| {
        task.parent_ids.is_empty()
            || task
                .parent_ids
                .iter()
                .all(|parent_id| !known_ids.contains(parent_id))
    };

    let mut rows = Vec::new();
    for task in tasks {
        if is_root(task) {
            visit(
                task,
                &children_by_parent,
                type_labels,
                user_names,
                &mut rows,
            );
        }
    }
    rows
}

/// Emits a `TaskRow` for `root` and every descendant reachable from it, in
/// pre-order, input order among siblings.
///
/// Iterative (an explicit work-stack), not recursive, matching
/// `bala-core`'s own convention for arbitrary-depth hierarchy walks (see
/// `hierarchy::check_new_parent`'s and `facade::tombstone_subtree`'s doc
/// comments): AC1 promises no fixed nesting depth limit, so this walk must
/// not be bounded by the process stack either. Each stack entry carries the
/// length `ancestors_on_path` should be truncated back to before visiting
/// it, so backtracking to a sibling branch correctly forgets the ancestors
/// only visible on the branch just finished — a cycle, which should never
/// occur given `Core::set_parents`'s invariant but would otherwise walk
/// forever if that invariant were ever violated by a bug, is detected via
/// `ancestors_on_path` and simply stops that path rather than looping.
fn visit(
    root: &Task,
    children_by_parent: &HashMap<TaskId, Vec<&Task>>,
    type_labels: &HashMap<String, String>,
    user_names: &HashMap<UserId, String>,
    rows: &mut Vec<TaskRow>,
) {
    let mut ancestors_on_path: Vec<TaskId> = Vec::new();
    let mut pending = vec![(root, 0usize, 0usize)];

    while let Some((task, depth, path_len)) = pending.pop() {
        ancestors_on_path.truncate(path_len);
        if ancestors_on_path.contains(&task.id) {
            continue;
        }

        rows.push(TaskRow {
            id: task.id,
            title: task.title.clone(),
            type_label: type_labels
                .get(&task.type_key)
                .cloned()
                .unwrap_or_else(|| task.type_key.clone()),
            status: task.status,
            assignee_name: task.assignee_id.and_then(|id| user_names.get(&id).cloned()),
            depth,
        });

        ancestors_on_path.push(task.id);
        let new_path_len = ancestors_on_path.len();
        if let Some(children) = children_by_parent.get(&task.id) {
            for child in children.iter().rev() {
                pending.push((child, depth + 1, new_path_len));
            }
        }
    }
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
    fn task_rows_should_set_depth_zero_for_top_level_tasks() {
        let top_level = task(TaskId::new(), "Top level", vec![]);
        let tasks = vec![top_level.clone()];

        let rows = task_rows(&tasks, &HashMap::new(), &HashMap::new());

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, top_level.id);
        assert_eq!(rows[0].title, "Top level");
        assert_eq!(rows[0].depth, 0);
    }

    #[test]
    fn task_rows_should_nest_child_directly_after_its_parent_with_depth_one() {
        let parent = task(TaskId::new(), "Parent", vec![]);
        let child = task(TaskId::new(), "Child", vec![parent.id]);
        let tasks = vec![parent.clone(), child.clone()];

        let rows = task_rows(&tasks, &HashMap::new(), &HashMap::new());

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, parent.id);
        assert_eq!(rows[0].depth, 0);
        assert_eq!(rows[1].id, child.id);
        assert_eq!(rows[1].depth, 1);
    }

    #[test]
    fn task_rows_should_nest_multiple_levels() {
        let grandparent = task(TaskId::new(), "Grandparent", vec![]);
        let parent = task(TaskId::new(), "Parent", vec![grandparent.id]);
        let child = task(TaskId::new(), "Child", vec![parent.id]);
        let tasks = vec![grandparent.clone(), parent.clone(), child.clone()];

        let rows = task_rows(&tasks, &HashMap::new(), &HashMap::new());

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].id, grandparent.id);
        assert_eq!(rows[0].depth, 0);
        assert_eq!(rows[1].id, parent.id);
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows[2].id, child.id);
        assert_eq!(rows[2].depth, 2);
    }

    #[test]
    fn task_rows_should_not_overflow_the_stack_on_a_deep_chain() {
        // Regression test for the iterative (not recursive) DFS walk: AC1
        // promises no fixed nesting depth limit, so a chain deep enough to
        // blow a recursive call stack must still render correctly.
        let mut tasks = Vec::new();
        let mut parent_id = None;
        for i in 0..5000 {
            let id = TaskId::new();
            tasks.push(task(
                id,
                &format!("Task {i}"),
                parent_id.into_iter().collect(),
            ));
            parent_id = Some(id);
        }

        let rows = task_rows(&tasks, &HashMap::new(), &HashMap::new());

        assert_eq!(rows.len(), 5000);
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(row.depth, i);
        }
    }

    #[test]
    fn task_rows_should_list_a_multi_parent_task_once_under_each_parent() {
        let parent_a = task(TaskId::new(), "Parent A", vec![]);
        let parent_b = task(TaskId::new(), "Parent B", vec![]);
        let child = task(
            TaskId::new(),
            "Shared child",
            vec![parent_a.id, parent_b.id],
        );
        let tasks = vec![parent_a.clone(), parent_b.clone(), child.clone()];

        let rows = task_rows(&tasks, &HashMap::new(), &HashMap::new());

        // Roots visited in input order (Parent A, then Parent B); each
        // root's subtree is fully walked (pre-order) before moving to the
        // next root, so the shared child appears once under each parent.
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].id, parent_a.id);
        assert_eq!(rows[0].depth, 0);
        assert_eq!(rows[1].id, child.id);
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows[2].id, parent_b.id);
        assert_eq!(rows[2].depth, 0);
        assert_eq!(rows[3].id, child.id);
        assert_eq!(rows[3].depth, 1);
    }

    #[test]
    fn task_rows_should_render_orphaned_child_as_root_when_its_parent_is_missing_from_input() {
        let parent_id = TaskId::new();
        let child_a = task(TaskId::new(), "Child A", vec![parent_id]);
        let child_b = task(TaskId::new(), "Child B", vec![parent_id]);
        let tasks = vec![child_a.clone(), child_b.clone()];

        let rows = task_rows(&tasks, &HashMap::new(), &HashMap::new());

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, child_a.id);
        assert_eq!(rows[0].depth, 0);
        assert_eq!(rows[1].id, child_b.id);
        assert_eq!(rows[1].depth, 0);
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
}
