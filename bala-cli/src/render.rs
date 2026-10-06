//! Pure task-row projection for the TUI's task list view.
//!
//! `task_rows` turns already-fetched `Task`s (plus small lookup maps for
//! type labels and user names) into `TaskRow`s ready for display. It does no
//! I/O and never touches `Core` itself — the TUI's `app` and `screens`
//! modules call it after fetching data through `Core`, which
//! keeps this projection unit-testable without a terminal or a database.
//!
//! `subtree_ids` and `dependents_of` are pure scans over the same
//! `Core::get_tree()` result, used by `bala task delete` and the TUI's `dd`
//! prompt to name the tasks that depend on what they delete, and by the
//! TUI's Detail pane to list the tasks that depend on the selected one.

use std::collections::{HashMap, HashSet};

use bala_core::{SiblingOrder, Task, TaskId, TaskStatus, UserId};

/// One row of the task list view: a task's display-ready fields, plus its
/// nesting depth in the tree. Every task has exactly one row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRow {
    pub id: TaskId,
    pub title: String,
    pub type_label: String,
    pub status: TaskStatus,
    pub assignee_name: Option<String>,
    pub depth: usize,
    /// Whether this task has at least one direct child in the input.
    pub has_children: bool,
    /// Whether this task is currently collapsed (its children, if any, are
    /// hidden from the rendered rows).
    pub collapsed: bool,
    /// `Some((complete_count, total_count))` counting this task's *direct*
    /// children only (not recursive descendants), present only when the row
    /// is both collapsed and has children; `None` otherwise.
    pub direct_summary: Option<(usize, usize)>,
    /// The parent this row is rendered under (`None` for a root row). This
    /// is the task's own parent unless that parent is absent from the input
    /// (e.g. hidden by a type filter), in which case the row is a root.
    pub parent_id: Option<TaskId>,
    /// Whether the task is waiting on at least one incomplete predecessor,
    /// as reported by Core in the task's `blocked_by`.
    pub blocked: bool,
}

/// Projects every task in `tasks` into a display-ready `TaskRow`, nested
/// under its parent.
///
/// Rows are produced by a pre-order depth-first walk starting from every
/// "root" task, where a root is a task with no `parent_id`, or whose
/// parent is absent from `tasks` (defensive against a partial
/// input — see below). Roots are visited in `sibling_order.children_of(None)`
/// order, and each task's children in `children_of(Some(parent))` order
/// (input order is only the fallback for tasks it doesn't list). Each task
/// is emitted as one row, at the depth of its place in its parent's
/// subtree.
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
///
/// `collapsed` names tasks whose children should be hidden from the
/// rendered rows: a collapsed task still gets its own row (with
/// `has_children`/`direct_summary` computed from its direct children), but
/// none of its descendants are visited, so they don't appear as rows at all.
#[must_use]
pub fn task_rows(
    tasks: &[Task],
    sibling_order: &SiblingOrder,
    type_labels: &HashMap<String, String>,
    user_names: &HashMap<UserId, String>,
    collapsed: &HashSet<TaskId>,
) -> Vec<TaskRow> {
    let known_ids: HashSet<TaskId> = tasks.iter().map(|task| task.id).collect();

    let mut children_by_parent: HashMap<TaskId, Vec<&Task>> = HashMap::new();
    for task in tasks {
        if let Some(parent_id) = task.parent_id {
            children_by_parent.entry(parent_id).or_default().push(task);
        }
    }

    // Order each parent's children by the stored sibling order; a child the
    // order doesn't list (defensive) sorts last, keeping input order.
    for (parent_id, children) in &mut children_by_parent {
        let position = positions(sibling_order.children_of(Some(*parent_id)));
        children.sort_by_key(|child| position.get(&child.id).copied().unwrap_or(usize::MAX));
    }
    let root_position = positions(sibling_order.children_of(None));

    let is_root = |task: &Task| {
        task.parent_id
            .is_none_or(|parent_id| !known_ids.contains(&parent_id))
    };

    // Descendants hidden by a collapsed ancestor are deliberately not
    // rendered, but they must not be mistaken by the fallback loop below for
    // tasks that were never reached at all — `visit` records every id it
    // chose not to descend into here so the fallback loop can skip them too.
    let mut hidden_by_collapse: HashSet<TaskId> = HashSet::new();

    let mut roots: Vec<&Task> = tasks.iter().filter(|task| is_root(task)).collect();
    roots.sort_by_key(|task| root_position.get(&task.id).copied().unwrap_or(usize::MAX));

    let mut rows = Vec::new();
    for task in roots {
        visit(
            task,
            &children_by_parent,
            type_labels,
            user_names,
            collapsed,
            &mut rows,
            &mut hidden_by_collapse,
        );
    }

    // Defensive fallback: if every task in `tasks` forms a cycle among
    // themselves (each one's `parent_id` names another task also present
    // in `tasks`), `is_root` is false for all of them and the loop above
    // renders nothing at all, silently disappearing every task rather than
    // just failing to indent it correctly. This should never arise through
    // `Core::set_parent`'s invariant, but the same defensive posture this
    // function already takes for a missing parent (see `is_root`'s doc
    // comment) applies here too: any task not reached by a normal root's
    // walk is rendered as its own root instead of vanishing. `rendered_ids`
    // is updated as we go (not just computed once) so a cyclic component
    // rendered by an earlier iteration's `visit` call isn't rendered a
    // second time when this loop reaches its other members. Ids hidden by a
    // collapsed ancestor are seeded in up front so they aren't re-rendered
    // as spurious roots.
    let mut rendered_ids: HashSet<TaskId> = rows.iter().map(|row| row.id).collect();
    rendered_ids.extend(hidden_by_collapse.iter().copied());
    for task in tasks {
        if rendered_ids.contains(&task.id) {
            continue;
        }
        let before = rows.len();
        visit(
            task,
            &children_by_parent,
            type_labels,
            user_names,
            collapsed,
            &mut rows,
            &mut hidden_by_collapse,
        );
        rendered_ids.extend(rows[before..].iter().map(|row| row.id));
        rendered_ids.extend(hidden_by_collapse.iter().copied());
    }

    rows
}

/// Maps each id in `ids` to its index.
fn positions(ids: &[TaskId]) -> HashMap<TaskId, usize> {
    ids.iter().enumerate().map(|(i, id)| (*id, i)).collect()
}

/// Emits a `TaskRow` for `root` and every descendant reachable from it, in
/// pre-order, input order among siblings.
///
/// Iterative (an explicit work-stack), not recursive, matching
/// `bala-core`'s own convention for arbitrary-depth hierarchy walks (see
/// `hierarchy::check_new_parent`'s and `facade::tombstone_subtree`'s doc
/// comments): hierarchy depth is deliberately unbounded, so this walk must
/// not be bounded by the process stack either. Each stack entry carries the
/// length `ancestors_on_path` should be truncated back to before visiting
/// it, so backtracking to a sibling branch correctly forgets the ancestors
/// only visible on the branch just finished — a cycle, which should never
/// occur given `Core::set_parent`'s invariant but would otherwise walk
/// forever if that invariant were ever violated by a bug, is detected via
/// `ancestors_on_path` and simply stops that path rather than looping.
fn visit(
    root: &Task,
    children_by_parent: &HashMap<TaskId, Vec<&Task>>,
    type_labels: &HashMap<String, String>,
    user_names: &HashMap<UserId, String>,
    collapsed: &HashSet<TaskId>,
    rows: &mut Vec<TaskRow>,
    hidden_by_collapse: &mut HashSet<TaskId>,
) {
    let mut ancestors_on_path: Vec<TaskId> = Vec::new();
    let mut pending = vec![(root, 0usize, 0usize)];

    while let Some((task, depth, path_len)) = pending.pop() {
        ancestors_on_path.truncate(path_len);
        if ancestors_on_path.contains(&task.id) {
            continue;
        }

        let children = children_by_parent.get(&task.id);
        let has_children = children.is_some_and(|children| !children.is_empty());
        let is_collapsed = collapsed.contains(&task.id);
        let direct_summary = (has_children && is_collapsed).then(|| {
            let children = children.expect("has_children implies children is Some");
            let complete = children
                .iter()
                .filter(|child| child.status == TaskStatus::Complete)
                .count();
            (complete, children.len())
        });

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
            has_children,
            collapsed: is_collapsed,
            direct_summary,
            parent_id: ancestors_on_path.last().copied(),
            blocked: !task.blocked_by.is_empty(),
        });

        if is_collapsed {
            if let Some(children) = children {
                mark_hidden(children, children_by_parent, hidden_by_collapse);
            }
            continue;
        }

        ancestors_on_path.push(task.id);
        let new_path_len = ancestors_on_path.len();
        if let Some(children) = children {
            for child in children.iter().rev() {
                pending.push((child, depth + 1, new_path_len));
            }
        }
    }
}

/// Records every id reachable from `children` (its own ids, plus all of
/// their descendants) into `hidden_by_collapse`, so `task_rows`'s defensive
/// fallback loop doesn't mistake a task hidden under a collapsed ancestor
/// for one that was never reached at all.
///
/// Iterative, matching `visit`'s own convention, with a `visited` guard so a
/// cycle (which should never occur — see `visit`'s doc comment) can't loop
/// forever.
fn mark_hidden(
    children: &[&Task],
    children_by_parent: &HashMap<TaskId, Vec<&Task>>,
    hidden_by_collapse: &mut HashSet<TaskId>,
) {
    let mut pending: Vec<&Task> = children.to_vec();
    while let Some(task) = pending.pop() {
        if !hidden_by_collapse.insert(task.id) {
            continue;
        }
        if let Some(grandchildren) = children_by_parent.get(&task.id) {
            pending.extend(grandchildren.iter().copied());
        }
    }
}

/// Returns `id` and the id of every task below it at any depth, each once,
/// found through `tasks`' `parent_id` (a `Core::get_tree` result).
///
/// Iterative, with a `visited` guard, matching `visit`'s convention:
/// hierarchy depth is unbounded, and a cycle (which should never occur)
/// can't loop forever.
#[must_use]
pub(crate) fn subtree_ids(id: TaskId, tasks: &[Task]) -> Vec<TaskId> {
    let mut children_by_parent: HashMap<TaskId, Vec<TaskId>> = HashMap::new();
    for task in tasks {
        if let Some(parent) = task.parent_id {
            children_by_parent.entry(parent).or_default().push(task.id);
        }
    }

    let mut visited = HashSet::new();
    let mut ids = Vec::new();
    let mut pending = vec![id];
    while let Some(current) = pending.pop() {
        if !visited.insert(current) {
            continue;
        }
        ids.push(current);
        if let Some(children) = children_by_parent.get(&current) {
            pending.extend(children.iter().copied());
        }
    }
    ids
}

/// Returns the tasks in `tasks` whose `depends_on` names any of `ids`,
/// excluding tasks that are themselves in `ids`, in `tasks`' order.
///
/// This is the successor scan CLI/TUI LLD §Task Detail View describes for
/// "blocks", generalised to a set of predecessors.
#[must_use]
pub(crate) fn dependents_of<'a>(ids: &[TaskId], tasks: &'a [Task]) -> Vec<&'a Task> {
    let ids: HashSet<TaskId> = ids.iter().copied().collect();
    tasks
        .iter()
        .filter(|task| !ids.contains(&task.id))
        .filter(|task| {
            task.depends_on
                .iter()
                .any(|dependency| ids.contains(&dependency.predecessor_id))
        })
        .collect()
}

/// Test helper: a `SiblingOrder` following `tasks`' input order (roots under
/// `None`, each parent's children in input order).
#[cfg(test)]
pub(crate) fn order_of(tasks: &[Task]) -> SiblingOrder {
    let mut map: HashMap<Option<TaskId>, Vec<TaskId>> = HashMap::new();
    for task in tasks {
        map.entry(task.parent_id).or_default().push(task.id);
    }
    SiblingOrder::from(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bala_core::{Dependency, DependencyType};
    use chrono::Utc;

    /// Minimal `Task` fixture: fills required fields with dummy values so
    /// each test only needs to override what it cares about.
    fn task(id: TaskId, title: &str, parent_id: Option<TaskId>) -> Task {
        let now = Utc::now();
        Task {
            id,
            title: title.to_string(),
            description: None,
            parent_id,
            type_key: "task".to_string(),
            status: TaskStatus::Incomplete,
            progress: 0.0,
            start_date: None,
            due_date: None,
            duration_days: None,
            dates_fixed: false,
            assignee_id: None,
            depends_on: Vec::new(),
            out_of_sync: false,
            blocked_by: Vec::new(),
            created_at: now,
            updated_at: now,
            deleted_at: None,
            completed_at: None,
        }
    }

    fn order(entries: &[(Option<TaskId>, &[TaskId])]) -> SiblingOrder {
        SiblingOrder::from(
            entries
                .iter()
                .map(|(k, v)| (*k, v.to_vec()))
                .collect::<HashMap<_, _>>(),
        )
    }

    fn rows_of(tasks: &[Task], order: &SiblingOrder) -> Vec<TaskRow> {
        task_rows(
            tasks,
            order,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        )
    }

    #[test]
    fn task_rows_should_order_children_by_sibling_order() {
        let p = task(TaskId::new(), "P", None);
        let a = task(TaskId::new(), "A", Some(p.id));
        let b = task(TaskId::new(), "B", Some(p.id));
        let tasks = vec![p.clone(), a.clone(), b.clone()];
        let o = order(&[(None, &[p.id]), (Some(p.id), &[b.id, a.id])]);

        let ids: Vec<_> = rows_of(&tasks, &o).iter().map(|r| r.id).collect();

        assert_eq!(ids, [p.id, b.id, a.id]);
    }

    #[test]
    fn task_rows_should_order_roots_by_none_key() {
        let a = task(TaskId::new(), "A", None);
        let b = task(TaskId::new(), "B", None);
        let tasks = vec![a.clone(), b.clone()];
        let o = order(&[(None, &[b.id, a.id])]);

        let ids: Vec<_> = rows_of(&tasks, &o).iter().map(|r| r.id).collect();

        assert_eq!(ids, [b.id, a.id]);
    }

    #[test]
    fn task_rows_should_expose_parent_id() {
        let r = task(TaskId::new(), "R", None);
        let m = task(TaskId::new(), "M", Some(r.id));
        let l = task(TaskId::new(), "L", Some(m.id));
        let tasks = vec![r.clone(), m.clone(), l.clone()];

        let rows = rows_of(&tasks, &order_of(&tasks));

        let parents: Vec<_> = rows.iter().map(|row| row.parent_id).collect();
        assert_eq!(parents, [None, Some(r.id), Some(m.id)]);
    }

    #[test]
    fn task_rows_should_set_depth_zero_for_top_level_tasks() {
        let top_level = task(TaskId::new(), "Top level", None);
        let tasks = vec![top_level.clone()];

        let rows = task_rows(
            &tasks,
            &order_of(&tasks),
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        );

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, top_level.id);
        assert_eq!(rows[0].title, "Top level");
        assert_eq!(rows[0].depth, 0);
    }

    #[test]
    fn task_rows_should_nest_child_directly_after_its_parent_with_depth_one() {
        let parent = task(TaskId::new(), "Parent", None);
        let child = task(TaskId::new(), "Child", Some(parent.id));
        let tasks = vec![parent.clone(), child.clone()];

        let rows = task_rows(
            &tasks,
            &order_of(&tasks),
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        );

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, parent.id);
        assert_eq!(rows[0].depth, 0);
        assert_eq!(rows[1].id, child.id);
        assert_eq!(rows[1].depth, 1);
    }

    #[test]
    fn task_rows_should_nest_multiple_levels() {
        let grandparent = task(TaskId::new(), "Grandparent", None);
        let parent = task(TaskId::new(), "Parent", Some(grandparent.id));
        let child = task(TaskId::new(), "Child", Some(parent.id));
        let tasks = vec![grandparent.clone(), parent.clone(), child.clone()];

        let rows = task_rows(
            &tasks,
            &order_of(&tasks),
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        );

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
        // Regression test for the iterative (not recursive) DFS walk:
        // hierarchy depth is deliberately unbounded, so a chain deep enough to
        // blow a recursive call stack must still render correctly.
        let mut tasks = Vec::new();
        let mut parent_id = None;
        for i in 0..5000 {
            let id = TaskId::new();
            tasks.push(task(id, &format!("Task {i}"), parent_id));
            parent_id = Some(id);
        }

        let rows = task_rows(
            &tasks,
            &order_of(&tasks),
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        );

        assert_eq!(rows.len(), 5000);
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(row.depth, i);
        }
    }

    #[test]
    fn task_rows_should_render_orphaned_child_as_root_when_its_parent_is_missing_from_input() {
        let parent_id = TaskId::new();
        let child_a = task(TaskId::new(), "Child A", Some(parent_id));
        let child_b = task(TaskId::new(), "Child B", Some(parent_id));
        let tasks = vec![child_a.clone(), child_b.clone()];

        let rows = task_rows(
            &tasks,
            &order_of(&tasks),
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        );

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, child_a.id);
        assert_eq!(rows[0].depth, 0);
        assert_eq!(rows[1].id, child_b.id);
        assert_eq!(rows[1].depth, 0);
    }

    #[test]
    fn task_rows_should_render_every_task_once_when_all_tasks_form_a_pure_cycle() {
        // Defensive regression: `Core::set_parent`'s invariant should make
        // this unreachable through normal use, but `task_rows` shouldn't
        // silently drop every task if it ever is (see the fallback loop's
        // doc comment in `task_rows` itself). A 2-cycle (A's parent is B,
        // B's parent is A) has no task whose `parent_id` is `None` or
        // wholly absent from the input, so `is_root` is false for both.
        let id_a = TaskId::new();
        let id_b = TaskId::new();
        let task_a = task(id_a, "A", Some(id_b));
        let task_b = task(id_b, "B", Some(id_a));
        let tasks = vec![task_a, task_b];

        let rows = task_rows(
            &tasks,
            &order_of(&tasks),
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        );

        assert_eq!(
            rows.len(),
            2,
            "each task in the cycle rendered exactly once"
        );
        let rendered_ids: HashSet<TaskId> = rows.iter().map(|row| row.id).collect();
        assert!(rendered_ids.contains(&id_a));
        assert!(rendered_ids.contains(&id_b));
    }

    #[test]
    fn task_rows_should_emit_each_task_exactly_once() {
        let goal = task(TaskId::new(), "Goal", None);
        let left = task(TaskId::new(), "Left", Some(goal.id));
        let left_leaf = task(TaskId::new(), "Left leaf", Some(left.id));
        let right = task(TaskId::new(), "Right", Some(goal.id));
        let right_branch = task(TaskId::new(), "Right branch", Some(right.id));
        let right_leaf_a = task(TaskId::new(), "Right leaf A", Some(right_branch.id));
        let right_leaf_b = task(TaskId::new(), "Right leaf B", Some(right_branch.id));
        let other_root = task(TaskId::new(), "Other root", None);
        let other_child = task(TaskId::new(), "Other child", Some(other_root.id));
        // Deliberately not in tree order, so the count can't pass by
        // echoing the input.
        let tasks = vec![
            right_leaf_b,
            other_child,
            left,
            goal,
            right_branch,
            other_root,
            left_leaf,
            right,
            right_leaf_a,
        ];

        let rows = task_rows(
            &tasks,
            &order_of(&tasks),
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        );

        assert_eq!(rows.len(), tasks.len(), "one row per task");
        for task in &tasks {
            let count = rows.iter().filter(|row| row.id == task.id).count();
            assert_eq!(count, 1, "{} should have exactly one row", task.title);
        }
    }

    #[test]
    fn task_rows_should_mark_a_task_with_blocked_by_as_blocked() {
        let predecessor = task(TaskId::new(), "Predecessor", None);
        let mut successor = task(TaskId::new(), "Successor", None);
        successor.blocked_by = vec![predecessor.id];
        let tasks = [predecessor, successor];

        let rows = rows_of(&tasks, &order_of(&tasks));

        assert!(rows[1].blocked);
    }

    #[test]
    fn task_rows_should_not_mark_an_unblocked_task() {
        let predecessor = task(TaskId::new(), "Predecessor", None);
        let mut successor = task(TaskId::new(), "Successor", None);
        successor.depends_on = vec![bala_core::Dependency {
            predecessor_id: predecessor.id,
            dep_type: bala_core::DependencyType::default(),
        }];
        let tasks = [predecessor, successor];

        let rows = rows_of(&tasks, &order_of(&tasks));

        assert!(rows.iter().all(|row| !row.blocked));
    }

    #[test]
    fn task_rows_should_include_title_type_label_and_status() {
        let mut input = task(TaskId::new(), "Ship the thing", None);
        input.type_key = "initiative".to_string();
        input.status = TaskStatus::Complete;
        let type_labels: HashMap<String, String> =
            [("initiative".to_string(), "Initiative".to_string())]
                .into_iter()
                .collect();

        let order = order_of(std::slice::from_ref(&input));
        let rows = task_rows(
            std::slice::from_ref(&input),
            &order,
            &type_labels,
            &HashMap::new(),
            &HashSet::new(),
        );

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Ship the thing");
        assert_eq!(rows[0].type_label, "Initiative");
        assert_eq!(rows[0].status, TaskStatus::Complete);
    }

    #[test]
    fn task_rows_should_show_assignee_name_when_assigned() {
        let user_id = UserId::new();
        let mut input = task(TaskId::new(), "Assigned task", None);
        input.assignee_id = Some(user_id);
        let user_names: HashMap<UserId, String> = [(user_id, "Ada Lovelace".to_string())]
            .into_iter()
            .collect();

        let order = order_of(std::slice::from_ref(&input));
        let rows = task_rows(
            std::slice::from_ref(&input),
            &order,
            &HashMap::new(),
            &user_names,
            &HashSet::new(),
        );

        assert_eq!(rows[0].assignee_name, Some("Ada Lovelace".to_string()));
    }

    #[test]
    fn task_rows_should_show_unassigned_when_no_assignee() {
        let input = task(TaskId::new(), "Unassigned task", None);

        let order = order_of(std::slice::from_ref(&input));
        let rows = task_rows(
            std::slice::from_ref(&input),
            &order,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        );

        assert_eq!(rows[0].assignee_name, None);
    }

    #[test]
    fn task_rows_should_mark_task_with_children_as_has_children() {
        let parent = task(TaskId::new(), "Parent", None);
        let child = task(TaskId::new(), "Child", Some(parent.id));
        let tasks = vec![parent.clone(), child.clone()];

        let rows = task_rows(
            &tasks,
            &order_of(&tasks),
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        );

        assert_eq!(rows.len(), 2);
        assert!(rows[0].has_children);
        assert!(!rows[1].has_children);
    }

    #[test]
    fn task_rows_should_hide_descendants_of_a_collapsed_task() {
        let parent = task(TaskId::new(), "Parent", None);
        let child = task(TaskId::new(), "Child", Some(parent.id));
        let grandchild = task(TaskId::new(), "Grandchild", Some(child.id));
        let tasks = vec![parent.clone(), child.clone(), grandchild.clone()];
        let collapsed: HashSet<TaskId> = [parent.id].into_iter().collect();

        let rows = task_rows(
            &tasks,
            &order_of(&tasks),
            &HashMap::new(),
            &HashMap::new(),
            &collapsed,
        );

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, parent.id);
        assert!(rows[0].has_children);
        assert!(rows[0].collapsed);
    }

    #[test]
    fn task_rows_should_compute_direct_child_summary_for_collapsed_parent() {
        let parent = task(TaskId::new(), "Parent", None);
        let mut child_a = task(TaskId::new(), "Child A", Some(parent.id));
        child_a.status = TaskStatus::Complete;
        let mut child_b = task(TaskId::new(), "Child B", Some(parent.id));
        child_b.status = TaskStatus::Complete;
        let child_c = task(TaskId::new(), "Child C", Some(parent.id));
        let tasks = vec![
            parent.clone(),
            child_a.clone(),
            child_b.clone(),
            child_c.clone(),
        ];
        let collapsed: HashSet<TaskId> = [parent.id].into_iter().collect();

        let rows = task_rows(
            &tasks,
            &order_of(&tasks),
            &HashMap::new(),
            &HashMap::new(),
            &collapsed,
        );

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].direct_summary, Some((2, 3)));
    }

    #[test]
    fn task_rows_should_leave_expanded_parent_with_no_summary() {
        let parent = task(TaskId::new(), "Parent", None);
        let mut child_a = task(TaskId::new(), "Child A", Some(parent.id));
        child_a.status = TaskStatus::Complete;
        let mut child_b = task(TaskId::new(), "Child B", Some(parent.id));
        child_b.status = TaskStatus::Complete;
        let child_c = task(TaskId::new(), "Child C", Some(parent.id));
        let tasks = vec![
            parent.clone(),
            child_a.clone(),
            child_b.clone(),
            child_c.clone(),
        ];

        let rows = task_rows(
            &tasks,
            &order_of(&tasks),
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        );

        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].direct_summary, None);
    }

    #[test]
    fn task_rows_should_leave_leaf_task_with_no_summary() {
        let leaf = task(TaskId::new(), "Leaf", None);
        let collapsed: HashSet<TaskId> = [leaf.id].into_iter().collect();

        let rows = task_rows(
            std::slice::from_ref(&leaf),
            &order_of(std::slice::from_ref(&leaf)),
            &HashMap::new(),
            &HashMap::new(),
            &collapsed,
        );

        assert_eq!(rows.len(), 1);
        assert!(!rows[0].has_children);
        assert_eq!(rows[0].direct_summary, None);
    }

    #[test]
    fn task_rows_should_render_only_root_when_collapsing_root_of_a_deep_chain() {
        // Depth-regression extension of the 5000-node chain test above: even
        // when collapsing the root of a very deep chain, the summary must be
        // computed directly from `children_by_parent` (not by recursing into
        // descendants), so this must not overflow the stack either.
        let mut tasks = Vec::new();
        let mut parent_id = None;
        for i in 0..5000 {
            let id = TaskId::new();
            tasks.push(task(id, &format!("Task {i}"), parent_id));
            parent_id = Some(id);
        }
        let root_id = tasks[0].id;
        let collapsed: HashSet<TaskId> = [root_id].into_iter().collect();

        let rows = task_rows(
            &tasks,
            &order_of(&tasks),
            &HashMap::new(),
            &HashMap::new(),
            &collapsed,
        );

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, root_id);
        assert!(rows[0].collapsed);
        assert!(rows[0].has_children);
        assert_eq!(rows[0].direct_summary, Some((0, 1)));
    }

    #[test]
    fn render_task_rows_direct_summary_should_agree_with_core_progress_field() {
        // Regression test tying this module's own `direct_summary`
        // `(complete, total)` counting to `bala_core`'s independently
        // implemented `Task::progress` rollup (`rollup::direct_children_progress`,
        // driven through `Core`), so the two "how many direct children are
        // complete" computations can't silently drift apart.
        use bala_core::{Core, InMemoryStore, NewTask, TreeFilter};

        fn new_task(title: &str, parent_id: Option<TaskId>) -> NewTask {
            NewTask {
                title: title.to_owned(),
                description: None,
                parent_id,
                type_key: None,
                start_date: None,
                due_date: None,
                duration_days: None,
                assignee_id: None,
            }
        }

        let mut core = Core::new(InMemoryStore::default()).unwrap();
        let parent = core.create_task(new_task("Parent", None)).unwrap();
        let child_a = core
            .create_task(new_task("Child A", Some(parent.id)))
            .unwrap();
        core.complete_task(child_a.id, false).unwrap();
        core.create_task(new_task("Child B", Some(parent.id)))
            .unwrap();
        core.create_task(new_task("Child C", Some(parent.id)))
            .unwrap();

        let core_progress = core.get_task(parent.id).unwrap().unwrap().progress;

        let tasks = core.get_tree(TreeFilter::default()).unwrap();
        let collapsed: HashSet<TaskId> = [parent.id].into_iter().collect();
        let rows = task_rows(
            &tasks,
            &order_of(&tasks),
            &HashMap::new(),
            &HashMap::new(),
            &collapsed,
        );
        let parent_row = rows.iter().find(|row| row.id == parent.id).unwrap();
        let (complete, total) = parent_row
            .direct_summary
            .expect("collapsed parent with children has a summary");

        #[allow(clippy::cast_precision_loss)]
        let cli_progress = complete as f32 / total as f32;

        assert!(
            (cli_progress - core_progress).abs() < f32::EPSILON,
            "CLI direct_summary ({complete}/{total} = {cli_progress}) disagreed with \
             Core::Task::progress ({core_progress})"
        );
    }

    /// A finish-to-start dependency on `predecessor_id`.
    fn on(predecessor_id: TaskId) -> Dependency {
        Dependency {
            predecessor_id,
            dep_type: DependencyType::FinishToStart,
        }
    }

    #[test]
    fn dependents_of_should_return_tasks_depending_on_any_given_id() {
        let a = task(TaskId::new(), "A", None);
        let b = task(TaskId::new(), "B", None);
        let mut on_a = task(TaskId::new(), "On A", None);
        on_a.depends_on = vec![on(a.id)];
        let mut on_both = task(TaskId::new(), "On both", None);
        on_both.depends_on = vec![on(a.id), on(b.id)];
        let mut on_b = task(TaskId::new(), "On B", None);
        on_b.depends_on = vec![on(b.id)];
        let unrelated = task(TaskId::new(), "Unrelated", None);
        let tasks = vec![
            a.clone(),
            b.clone(),
            on_a.clone(),
            on_both.clone(),
            unrelated,
            on_b.clone(),
        ];

        let ids: Vec<_> = dependents_of(&[a.id, b.id], &tasks)
            .iter()
            .map(|t| t.id)
            .collect();

        assert_eq!(ids, vec![on_a.id, on_both.id, on_b.id]);
    }

    #[test]
    fn dependents_of_should_skip_tasks_inside_the_given_set() {
        let a = task(TaskId::new(), "A", None);
        let mut inside = task(TaskId::new(), "Inside", None);
        inside.depends_on = vec![on(a.id)];
        let mut outside = task(TaskId::new(), "Outside", None);
        outside.depends_on = vec![on(a.id)];
        let tasks = vec![a.clone(), inside.clone(), outside.clone()];

        let ids: Vec<_> = dependents_of(&[a.id, inside.id], &tasks)
            .iter()
            .map(|t| t.id)
            .collect();

        assert_eq!(ids, vec![outside.id]);
    }

    #[test]
    fn subtree_ids_should_collect_every_depth_once() {
        let root = task(TaskId::new(), "Root", None);
        let left = task(TaskId::new(), "Left", Some(root.id));
        let right = task(TaskId::new(), "Right", Some(root.id));
        let nested = task(TaskId::new(), "Nested", Some(left.id));
        let grandchild = task(TaskId::new(), "Grandchild", Some(nested.id));
        let outside = task(TaskId::new(), "Outside", None);
        let tasks = vec![
            root.clone(),
            left.clone(),
            right.clone(),
            nested.clone(),
            grandchild.clone(),
            outside,
        ];

        let ids = subtree_ids(root.id, &tasks);

        assert_eq!(ids.len(), 5, "each task appears once: {ids:?}");
        let expected: HashSet<_> = [root.id, left.id, right.id, nested.id, grandchild.id]
            .into_iter()
            .collect();
        assert_eq!(ids.into_iter().collect::<HashSet<_>>(), expected);
    }
}
