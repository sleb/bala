//! Direct-children progress rollup (LLD §Algorithm 4).
//!
//! [`direct_children_progress`] computes a task's `progress` from its
//! *direct* children only: average each direct
//! child's own `status` flag (`Complete` -> `1.0`, `Incomplete` -> `0.0`).
//! Grandchildren never factor in — a child that is itself `Complete` but
//! has ten incomplete grandchildren still counts as a full `1.0` toward its
//! parent, because the parent looks only at that child's `status`, not at
//! the child's own (separately-rolled-up) `progress`. There is no
//! recursion and nothing to memoize: each task's `progress` is one flat
//! lookup of its direct children's `status`.

use crate::model::{Task, TaskStatus};

/// Computes a task's progress from its direct children's `status` alone:
/// averages `1.0` for each [`TaskStatus::Complete`] child and `0.0` for each [`TaskStatus::Incomplete`] child, ignoring every child's
/// own `progress` field and never looking past direct children.
///
/// When `children` is empty, returns `1.0` if `own_status` is
/// [`TaskStatus::Complete`] else `0.0` — a leaf's progress is just
/// its own completion state. Once there is at least one child, `own_status`
/// plays no further role: the result is governed purely by the children's
/// statuses, independent of whatever `own_status` happens to be.
pub(crate) fn direct_children_progress(children: &[Task], own_status: TaskStatus) -> f32 {
    if children.is_empty() {
        return match own_status {
            TaskStatus::Complete => 1.0,
            TaskStatus::Incomplete => 0.0,
        };
    }

    let complete_count = children
        .iter()
        .filter(|c| c.status == TaskStatus::Complete)
        .count();

    #[allow(clippy::cast_precision_loss)]
    let progress = complete_count as f32 / children.len() as f32;
    progress
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TaskId;
    use chrono::Utc;

    fn task_with_status(status: TaskStatus) -> Task {
        let now = Utc::now();
        Task {
            id: TaskId::new(),
            title: "Task".to_owned(),
            description: None,
            parent_ids: Vec::new(),
            type_key: "task".to_owned(),
            status,
            progress: match status {
                TaskStatus::Complete => 1.0,
                TaskStatus::Incomplete => 0.0,
            },
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
    fn direct_children_progress_should_average_child_status_ignoring_grandchildren() {
        // Epic/story/subtask example: a "parent" task has
        // two direct children ("stories"), each itself Complete despite
        // having ten incomplete grandchildren ("subtasks") of its own. The
        // grandchildren's incompleteness must never leak two levels up —
        // only the direct children's own `status` (both Complete) counts.
        let mut story_a = task_with_status(TaskStatus::Complete);
        // A grandchild-derived `progress` value that, if wrongly consulted
        // instead of `status`, would drag the parent's rollup down.
        story_a.progress = 0.0;
        let mut story_b = task_with_status(TaskStatus::Complete);
        story_b.progress = 0.0;

        let children = [story_a, story_b];

        let progress = direct_children_progress(&children, TaskStatus::Incomplete);

        assert!((progress - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn direct_children_progress_should_return_own_status_when_no_children() {
        let no_children: [Task; 0] = [];

        let complete_progress = direct_children_progress(&no_children, TaskStatus::Complete);
        let incomplete_progress = direct_children_progress(&no_children, TaskStatus::Incomplete);

        assert!((complete_progress - 1.0).abs() < f32::EPSILON);
        assert!((incomplete_progress - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn direct_children_progress_should_be_independent_of_own_status() {
        // Two children, one Complete one Incomplete -> 0.5, regardless of
        // whether `own_status` is Complete or Incomplete. This directly
        // contradicts what a naive "return own status" implementation
        // would give once there's at least one child.
        let children = [
            task_with_status(TaskStatus::Complete),
            task_with_status(TaskStatus::Incomplete),
        ];

        let progress_when_own_incomplete =
            direct_children_progress(&children, TaskStatus::Incomplete);
        let progress_when_own_complete = direct_children_progress(&children, TaskStatus::Complete);

        assert!((progress_when_own_incomplete - 0.5).abs() < f32::EPSILON);
        assert!((progress_when_own_complete - 0.5).abs() < f32::EPSILON);
    }
}
