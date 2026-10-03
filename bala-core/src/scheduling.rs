//! Dependency invariants over the dependency-edge graph (LLD §Algorithm 2)
//! and the date constraint each dependency puts on its two tasks (LLD
//! §Algorithm 3).
//!
//! [`anchor_date`] and [`constraint_ok`] are the two per-type functions:
//! which of the predecessor's dates a dependency type reads, and whether a
//! successor's dates respect it. [`is_out_of_sync`] applies them to every
//! dependency of one task, and only reports a broken one. [`plan`] works
//! out the dates that would mend them, over a list of tasks it is handed:
//! it reads no store and writes nothing. `Core::preview_schedule` returns
//! its result as a proposal and `Core::reschedule` writes it.
//!
//! [`check_new_dependency`] is the gate every new dependency edge between
//! two distinct tasks passes before it is written; the caller rejects a
//! self-dependency first. It enforces two invariants: no task depends on
//! its own ancestor or descendant in the hierarchy, and no chain of
//! dependencies leads back to where it started. No walk recurses, as in
//! [`hierarchy::check_new_parent`](crate::hierarchy::check_new_parent):
//! hierarchy depth and dependency chains are unbounded, so a long enough
//! chain could overflow the process stack if they did. The hierarchy is a
//! tree, so its walks are plain loops up one parent chain; the dependency
//! graph is a real graph, so its walk is an explicit work-stack.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::convert::Infallible;

use chrono::{NaiveDate, TimeDelta};

use crate::error::CoreError;
use crate::model::{DependencyType, Rescheduled, Schedule, Task, TaskId, TaskStatus};
use crate::store::{StoreError, StoreTx};

/// The predecessor's date that a dependency of `dep_type` reads (LLD
/// §Algorithm 3): its due date for the finish-to-… types, its start date
/// for the start-to-… types. `None` when that date is unset.
pub fn anchor_date(dep_type: DependencyType, predecessor: &Task) -> Option<NaiveDate> {
    match dep_type {
        DependencyType::FinishToStart | DependencyType::FinishToFinish => predecessor.due_date,
        DependencyType::StartToStart | DependencyType::StartToFinish => predecessor.start_date,
    }
}

/// The successor's date that a dependency of `dep_type` constrains: its
/// start date for the …-to-start types, its due date for the …-to-finish
/// types. `None` when that date is unset.
fn constrained_date(dep_type: DependencyType, successor: &Task) -> Option<NaiveDate> {
    if constrains_start(dep_type) {
        successor.start_date
    } else {
        successor.due_date
    }
}

/// Whether a dependency of `dep_type` constrains its successor's start
/// date (the …-to-start types) rather than its due date.
fn constrains_start(dep_type: DependencyType) -> bool {
    match dep_type {
        DependencyType::FinishToStart | DependencyType::StartToStart => true,
        DependencyType::FinishToFinish | DependencyType::StartToFinish => false,
    }
}

/// Whether `successor`'s dates respect a dependency of `dep_type` on
/// `predecessor`: its constrained date is on or after the predecessor's
/// [`anchor_date`]. Falling on the same day is allowed. A dependency
/// constrains nothing while either of the two dates it reads is unset, so
/// this is `true` then.
pub fn constraint_ok(dep_type: DependencyType, predecessor: &Task, successor: &Task) -> bool {
    match (
        anchor_date(dep_type, predecessor),
        constrained_date(dep_type, successor),
    ) {
        (Some(anchor), Some(constrained)) => constrained >= anchor,
        _ => true,
    }
}

/// Whether `task`'s dates break at least one of its dependencies, read from
/// `task.depends_on`. Each predecessor is fetched with
/// [`StoreTx::get_task`] inside the caller's transaction, so a soft-deleted
/// one does not resolve and constrains nothing. A soft-deleted `task` is
/// never out of sync.
///
/// # Errors
///
/// Returns `Err` if the backend fails while fetching a predecessor.
pub fn is_out_of_sync(tx: &mut dyn StoreTx, task: &Task) -> Result<bool, StoreError> {
    breaks_a_dependency(task, |id| Ok(tx.get_task(id)?.map(Cow::Owned)))
}

/// [`is_out_of_sync`] over any source of live tasks: `live_task` resolves a
/// predecessor id to its task, or to `None` if it is soft-deleted or
/// missing. A caller that already holds some of the predecessors hands
/// them over borrowed instead of having them fetched again. Stops at the
/// first broken dependency.
///
/// # Errors
///
/// Returns whatever `live_task` fails with.
pub fn breaks_a_dependency<'a, E>(
    task: &Task,
    mut live_task: impl FnMut(TaskId) -> Result<Option<Cow<'a, Task>>, E>,
) -> Result<bool, E> {
    if task.deleted_at.is_some() {
        return Ok(false);
    }
    for dependency in &task.depends_on {
        if let Some(predecessor) = live_task(dependency.predecessor_id)?
            && !constraint_ok(dependency.dep_type, &predecessor, task)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Computes a schedule for `tasks` (LLD §Algorithm 3): the dates each
/// would have once every floating task is moved late enough to respect
/// its dependencies. Nothing is written; the result is a proposal.
///
/// The dependencies are read from each task's `depends_on`. Soft-deleted
/// tasks are ignored: they are not placed, not reported, and a dependency
/// on one constrains nothing, as does a dependency on a task that is not
/// in `tasks` at all.
///
/// Live tasks are placed one at a time in topological order of the
/// dependency graph, each after all of its predecessors and against the
/// dates those were just given, so a move carries on down a chain. Tasks
/// that are ready at the same time are placed in the order they are
/// listed. Only an incomplete task with floating dates is changed; a task
/// with fixed dates or a completed one keeps its dates, set or unset, and
/// its successors are placed from those. A start date that is set never
/// moves earlier. A due date follows the task's `duration_days`, so it can
/// move earlier when step 1 moves the start and the duration is shorter
/// than the span the task had. For a task that can move, with or without
/// predecessors:
///
/// 1. If its start date is unset, or earlier than the latest
///    finish-to-start or start-to-start anchor among its predecessors, the
///    start moves to that anchor and the due date follows at
///    `start + duration_days`, or, with no duration, at the distance it
///    had from the old start. With neither a duration nor both dates,
///    only the start is set. A start that is already late enough stays
///    where it is.
/// 2. If it now has a start, no due date and a duration, the due date is
///    set to `start + duration_days`.
/// 3. If its due date is still unset, it becomes the latest
///    finish-to-finish or start-to-finish anchor; if it is earlier than
///    that anchor, the start and the due date both move later by the
///    difference.
/// 4. If it now has a due date, no start and a duration, the start is set
///    to `due - duration_days`. A start is only still unset here when no
///    start anchor applied in step 1, so this never puts it before one.
/// 5. A due date left before the start is brought forward to the start.
///
/// Steps 2 and 4 only fill a date that is unset; neither moves a date the
/// task already has, and a task with a duration and no dates at all is
/// left alone unless a predecessor gives it one. The second run of `plan`
/// over its own result moves nothing.
///
/// [`Schedule::moved`] is in placement order, so a predecessor comes
/// before its successors. [`Schedule::out_of_sync`] is in the order of
/// `tasks`. The walk is a queue of tasks whose predecessors are all
/// placed, never a recursion, and puts each task on the queue at most
/// once, so it always ends. The graph has no cycles when its edges all
/// passed [`check_new_dependency`]; if it has one anyway, the tasks on it
/// and every task downstream of it are never ready, so they keep their
/// dates, and are reported in `out_of_sync` if those break a dependency.
#[must_use]
pub fn plan(mut tasks: Vec<Task>) -> Schedule {
    let index_of: HashMap<TaskId, usize> = tasks
        .iter()
        .enumerate()
        .filter(|(_, task)| task.deleted_at.is_none())
        .map(|(index, task)| (task.id, index))
        .collect();

    let flags = out_of_sync_flags(&tasks, &index_of);
    for (task, out_of_sync) in tasks.iter_mut().zip(flags) {
        task.out_of_sync = out_of_sync;
    }

    // `unplaced[i]` counts the live predecessors task `i` still waits for;
    // `successors[i]` lists the tasks waiting for task `i`, once per edge.
    let mut unplaced = vec![0_usize; tasks.len()];
    let mut successors = vec![Vec::new(); tasks.len()];
    for (index, task) in tasks.iter().enumerate() {
        if task.deleted_at.is_some() {
            continue;
        }
        for dependency in &task.depends_on {
            if let Some(&predecessor) = index_of.get(&dependency.predecessor_id) {
                unplaced[index] += 1;
                successors[predecessor].push(index);
            }
        }
    }

    let mut ready: VecDeque<usize> = (0..tasks.len())
        .filter(|&index| unplaced[index] == 0 && tasks[index].deleted_at.is_none())
        .collect();
    let mut moved = Vec::new();
    while let Some(index) = ready.pop_front() {
        if let Some((start, due)) = placement(&tasks[index], &tasks, &index_of) {
            let before = tasks[index].clone();
            tasks[index].start_date = start;
            tasks[index].due_date = due;
            moved.push((index, before));
        }
        for &successor in &successors[index] {
            unplaced[successor] -= 1;
            if unplaced[successor] == 0 {
                ready.push_back(successor);
            }
        }
    }

    let flags = out_of_sync_flags(&tasks, &index_of);
    for (task, out_of_sync) in tasks.iter_mut().zip(flags) {
        task.out_of_sync = out_of_sync;
    }
    Schedule {
        moved: moved
            .into_iter()
            .map(|(index, before)| Rescheduled {
                before,
                after: tasks[index].clone(),
            })
            .collect(),
        out_of_sync: tasks.into_iter().filter(|task| task.out_of_sync).collect(),
    }
}

/// For each of `tasks`, whether its dates break one of its dependencies as
/// the dates stand in `tasks`. `index_of` maps each live task's id to its
/// place in `tasks`.
fn out_of_sync_flags(tasks: &[Task], index_of: &HashMap<TaskId, usize>) -> Vec<bool> {
    tasks
        .iter()
        .map(|task| {
            let Ok(broken) = breaks_a_dependency(task, |id| {
                Ok::<_, Infallible>(index_of.get(&id).map(|&index| Cow::Borrowed(&tasks[index])))
            });
            broken
        })
        .collect()
}

/// The dates [`plan`] gives `task` against its predecessors as they stand
/// in `tasks`, or `None` if it stays where it is: it is not a live,
/// incomplete, floating task, or its dates already respect every
/// dependency and its duration has no unset date to fill.
fn placement(
    task: &Task,
    tasks: &[Task],
    index_of: &HashMap<TaskId, usize>,
) -> Option<(Option<NaiveDate>, Option<NaiveDate>)> {
    if task.deleted_at.is_some() || task.dates_fixed || task.status == TaskStatus::Complete {
        return None;
    }

    // The earliest each date may be: the latest anchor that constrains it.
    let (mut earliest_start, mut earliest_due) = (None, None);
    for dependency in &task.depends_on {
        let Some(&predecessor) = index_of.get(&dependency.predecessor_id) else {
            continue;
        };
        let anchor = anchor_date(dependency.dep_type, &tasks[predecessor]);
        let bound = if constrains_start(dependency.dep_type) {
            &mut earliest_start
        } else {
            &mut earliest_due
        };
        *bound = (*bound).max(anchor);
    }

    let (mut start, mut due) = (task.start_date, task.due_date);
    let duration = task
        .duration_days
        .map(|days| TimeDelta::days(i64::from(days)));

    if let Some(anchor) = earliest_start
        && start.is_none_or(|start| start < anchor)
    {
        let length = match (duration, start, due) {
            (Some(duration), _, _) => Some(duration),
            (None, Some(start), Some(due)) => Some(due - start),
            (None, _, _) => None,
        };
        start = Some(anchor);
        if let Some(length) = length {
            due = Some(later_by(anchor, length));
        }
    }

    // A missing due date is filled from the duration before the finish
    // anchors are applied, so they push a task of the right length.
    if let (Some(start), None, Some(duration)) = (start, due, duration) {
        due = Some(later_by(start, duration));
    }

    if let Some(anchor) = earliest_due {
        match due {
            None => due = Some(anchor),
            Some(current) if current < anchor => {
                start = start.map(|start| later_by(start, anchor - current));
                due = Some(anchor);
            }
            Some(_) => {}
        }
    }

    // A start still missing here had no start anchor to take, so filling
    // it from the duration cannot put it before one.
    if let (None, Some(due), Some(duration)) = (start, due, duration) {
        start = Some(earlier_by(due, duration));
    }

    if (start, due) == (task.start_date, task.due_date) {
        return None;
    }
    if let (Some(start), Some(current)) = (start, due)
        && current < start
    {
        due = Some(start);
    }
    Some((start, due))
}

/// `date` moved by `length`, stopping at the last date a [`NaiveDate`] can
/// hold rather than overflowing.
fn later_by(date: NaiveDate, length: TimeDelta) -> NaiveDate {
    date.checked_add_signed(length).unwrap_or(NaiveDate::MAX)
}

/// `date` moved back by `length`, stopping at the first date a
/// [`NaiveDate`] can hold rather than overflowing.
fn earlier_by(date: NaiveDate, length: TimeDelta) -> NaiveDate {
    date.checked_sub_signed(length).unwrap_or(NaiveDate::MIN)
}

/// Checks that making `id` depend on `predecessor`, a different task,
/// would not violate a dependency invariant. The caller rejects
/// `id == predecessor` with [`CoreError::SelfDependency`] before calling
/// this.
///
/// Both hierarchy checks follow one parent chain upward via
/// [`StoreTx::get_parent_edge`]: from `id` looking for `predecessor` (an
/// ancestor), then from `predecessor` looking for `id` (a descendant of
/// `id` is exactly a task from which `id` is reachable upward). Each walk
/// visits only the ancestors of its start, never `id`'s whole subtree.
/// Then walks the dependency graph from `predecessor` through
/// what it already depends on, transitively, via
/// [`StoreTx::list_dependency_edges`], and rejects if `id` turns up: the
/// new edge would close a cycle. That walk ignores dependency types, and
/// since soft-deleted tasks keep their edges, every walk passes through
/// them. Whether either task exists is the caller's concern.
///
/// # Errors
///
/// - [`CoreError::DependsOnRelative`] if `predecessor` is an ancestor or a
///   descendant of `id`.
/// - [`CoreError::CircularDependency`] if `predecessor` already depends on
///   `id`, directly or transitively; `cycle` is `[id, predecessor, …, id]`,
///   each entry depending on the next.
/// - [`CoreError::Store`] if the backend fails while walking either graph.
pub fn check_new_dependency(
    tx: &mut dyn StoreTx,
    id: TaskId,
    predecessor: TaskId,
) -> Result<(), CoreError> {
    if has_ancestor(tx, id, predecessor)? || has_ancestor(tx, predecessor, id)? {
        return Err(CoreError::DependsOnRelative {
            task: id,
            other: predecessor,
        });
    }

    let dependency_path = find_path(tx, predecessor, id, |tx, task| {
        Ok(tx
            .list_dependency_edges(task)?
            .into_iter()
            .map(|dependency| dependency.predecessor_id)
            .collect())
    })?;
    if let Some(path) = dependency_path {
        let mut cycle = Vec::with_capacity(path.len() + 1);
        cycle.push(id);
        cycle.extend(path);
        return Err(CoreError::CircularDependency { cycle });
    }

    Ok(())
}

/// Whether `ancestor` is on `task`'s parent chain, at any distance. A plain
/// loop up the one chain: a task has at most one parent, so there is
/// nothing to branch into and nothing to revisit.
fn has_ancestor(tx: &mut dyn StoreTx, task: TaskId, ancestor: TaskId) -> Result<bool, StoreError> {
    let mut current = tx.get_parent_edge(task)?;
    while let Some(parent) = current {
        if parent == ancestor {
            return Ok(true);
        }
        current = tx.get_parent_edge(parent)?;
    }
    Ok(false)
}

/// Finds a path from `start` to `target` by following `neighbors` one edge
/// at a time, returning it as `[start, …, target]`, or `None` if `target`
/// is unreachable.
///
/// The walk is an explicit work-stack seeded with `start`. Each task is
/// enqueued only the first time it is discovered, and the task it was
/// discovered from is recorded then, so a task reached through several
/// paths is walked exactly once and every recorded step is a real edge. The
/// walk stops as soon as `target` turns up, and the path is rebuilt by
/// following the recorded steps back to `start`.
fn find_path(
    tx: &mut dyn StoreTx,
    start: TaskId,
    target: TaskId,
    neighbors: impl Fn(&mut dyn StoreTx, TaskId) -> Result<Vec<TaskId>, StoreError>,
) -> Result<Option<Vec<TaskId>>, StoreError> {
    let mut discovered = HashSet::from([start]);
    let mut discovered_from = HashMap::new();
    let mut pending = vec![start];

    while let Some(current) = pending.pop() {
        if current == target {
            let mut path = vec![current];
            let mut node = current;
            while let Some(&previous) = discovered_from.get(&node) {
                path.push(previous);
                node = previous;
            }
            path.reverse();
            return Ok(Some(path));
        }

        for next in neighbors(tx, current)? {
            if discovered.insert(next) {
                discovered_from.insert(next, current);
                pending.push(next);
            }
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::in_memory_store::InMemoryStore;
    use crate::model::Dependency;
    use crate::store::Store;
    use chrono::{Datelike, Days, Utc};

    fn date(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 3, day).unwrap()
    }

    /// A task with these dates (days of March 2026) and nothing else set.
    fn task(start: Option<u32>, due: Option<u32>) -> Task {
        let now = Utc::now();
        Task {
            id: TaskId::new(),
            title: "Task".to_owned(),
            description: None,
            parent_id: None,
            type_key: "task".to_owned(),
            status: TaskStatus::Incomplete,
            progress: 0.0,
            start_date: start.map(date),
            due_date: due.map(date),
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

    // In each per-type test the predecessor runs 10..20 and the first
    // successor breaks only the pair of dates that type reads, while the
    // second satisfies that pair and breaks the other three.

    #[test]
    fn constraint_ok_should_compare_predecessor_due_to_successor_start_for_finish_to_start() {
        let dep_type = DependencyType::FinishToStart;
        let predecessor = task(Some(10), Some(20));

        assert!(!constraint_ok(
            dep_type,
            &predecessor,
            &task(Some(19), Some(30))
        ));
        assert!(constraint_ok(
            dep_type,
            &predecessor,
            &task(Some(21), Some(21))
        ));
        assert_eq!(anchor_date(dep_type, &predecessor), Some(date(20)));
    }

    #[test]
    fn constraint_ok_should_compare_predecessor_start_to_successor_start_for_start_to_start() {
        let dep_type = DependencyType::StartToStart;
        let predecessor = task(Some(10), Some(20));

        assert!(!constraint_ok(
            dep_type,
            &predecessor,
            &task(Some(9), Some(30))
        ));
        assert!(constraint_ok(
            dep_type,
            &predecessor,
            &task(Some(11), Some(12))
        ));
        assert_eq!(anchor_date(dep_type, &predecessor), Some(date(10)));
    }

    #[test]
    fn constraint_ok_should_compare_predecessor_due_to_successor_due_for_finish_to_finish() {
        let dep_type = DependencyType::FinishToFinish;
        let predecessor = task(Some(10), Some(20));

        assert!(!constraint_ok(
            dep_type,
            &predecessor,
            &task(Some(15), Some(19))
        ));
        assert!(constraint_ok(
            dep_type,
            &predecessor,
            &task(Some(5), Some(21))
        ));
        assert_eq!(anchor_date(dep_type, &predecessor), Some(date(20)));
    }

    #[test]
    fn constraint_ok_should_compare_predecessor_start_to_successor_due_for_start_to_finish() {
        let dep_type = DependencyType::StartToFinish;
        let predecessor = task(Some(10), Some(20));

        assert!(!constraint_ok(
            dep_type,
            &predecessor,
            &task(Some(5), Some(9))
        ));
        assert!(constraint_ok(
            dep_type,
            &predecessor,
            &task(Some(5), Some(11))
        ));
        assert_eq!(anchor_date(dep_type, &predecessor), Some(date(10)));
    }

    #[test]
    fn constraint_ok_should_allow_successor_to_start_on_the_predecessors_due_date() {
        let predecessor = task(Some(10), Some(20));
        let on_the_day = task(Some(20), Some(25));
        let day_before = task(Some(19), Some(25));

        assert!(constraint_ok(
            DependencyType::FinishToStart,
            &predecessor,
            &on_the_day
        ));
        assert!(!constraint_ok(
            DependencyType::FinishToStart,
            &predecessor,
            &day_before
        ));
    }

    #[test]
    fn constraint_ok_should_pass_when_a_date_it_reads_is_unset() {
        // Each pair would break the constraint if the unset date were any
        // day before the one it is compared with.
        let dated = task(Some(10), Some(20));
        let early = task(Some(1), Some(2));
        let no_start = task(None, Some(2));
        let no_due = task(Some(1), None);

        // Anchor unset: finish-to-start reads the predecessor's due date,
        // start-to-start its start date.
        assert!(constraint_ok(
            DependencyType::FinishToStart,
            &task(Some(10), None),
            &early
        ));
        assert!(constraint_ok(
            DependencyType::StartToStart,
            &task(None, Some(20)),
            &early
        ));
        // Constrained date unset: finish-to-start reads the successor's
        // start date, finish-to-finish its due date.
        assert!(constraint_ok(
            DependencyType::FinishToStart,
            &dated,
            &no_start
        ));
        assert!(constraint_ok(
            DependencyType::FinishToFinish,
            &dated,
            &no_due
        ));
        // The same pairs with the date present do break it.
        assert!(!constraint_ok(
            DependencyType::FinishToStart,
            &dated,
            &early
        ));
        assert!(!constraint_ok(
            DependencyType::FinishToFinish,
            &dated,
            &early
        ));
        assert_eq!(
            anchor_date(DependencyType::FinishToStart, &task(Some(10), None)),
            None
        );
    }

    // The `plan` tests build floating tasks (`task` leaves `dates_fixed`
    // off) and read a schedule back as day-of-March numbers.

    /// `task`, depending on each of `predecessors` with the given type.
    fn depending_on(mut task: Task, predecessors: &[(&Task, DependencyType)]) -> Task {
        task.depends_on = predecessors
            .iter()
            .map(|&(predecessor, dep_type)| Dependency {
                predecessor_id: predecessor.id,
                dep_type,
            })
            .collect();
        task
    }

    fn finish_to_start(task: Task, predecessor: &Task) -> Task {
        depending_on(task, &[(predecessor, DependencyType::FinishToStart)])
    }

    fn with_duration(mut task: Task, days: u32) -> Task {
        task.duration_days = Some(days);
        task
    }

    fn fixed(mut task: Task) -> Task {
        task.dates_fixed = true;
        task
    }

    fn deleted(mut task: Task) -> Task {
        task.deleted_at = Some(Utc::now());
        task
    }

    /// Each move as `(id, start, due)`, the dates being the scheduled ones
    /// as days of March 2026.
    fn moves(schedule: &Schedule) -> Vec<(TaskId, Option<u32>, Option<u32>)> {
        schedule
            .moved
            .iter()
            .map(|moved| {
                (
                    moved.after.id,
                    moved.after.start_date.map(|d| d.day()),
                    moved.after.due_date.map(|d| d.day()),
                )
            })
            .collect()
    }

    fn ids(tasks: &[Task]) -> Vec<TaskId> {
        tasks.iter().map(|task| task.id).collect()
    }

    /// `tasks` with every move in `schedule` applied.
    fn applied(mut tasks: Vec<Task>, schedule: &Schedule) -> Vec<Task> {
        for moved in &schedule.moved {
            let task = tasks
                .iter_mut()
                .find(|task| task.id == moved.after.id)
                .unwrap();
            task.start_date = moved.after.start_date;
            task.due_date = moved.after.due_date;
        }
        tasks
    }

    #[test]
    fn plan_should_move_start_and_due_together_for_finish_to_start() {
        let predecessor = task(Some(10), Some(20));
        let successor = finish_to_start(task(Some(15), Some(20)), &predecessor);
        let id = successor.id;

        let schedule = plan(vec![predecessor, successor]);

        assert_eq!(moves(&schedule), [(id, Some(20), Some(25))]);
        assert_eq!(schedule.out_of_sync, []);
    }

    #[test]
    fn plan_should_move_start_and_due_together_for_start_to_start() {
        let predecessor = task(Some(10), Some(20));
        let successor = depending_on(
            task(Some(5), Some(8)),
            &[(&predecessor, DependencyType::StartToStart)],
        );
        let id = successor.id;

        let schedule = plan(vec![predecessor, successor]);

        assert_eq!(moves(&schedule), [(id, Some(10), Some(13))]);
        assert_eq!(schedule.out_of_sync, []);
    }

    #[test]
    fn plan_should_move_start_and_due_together_for_finish_to_finish() {
        let predecessor = task(Some(10), Some(20));
        let successor = depending_on(
            task(Some(12), Some(15)),
            &[(&predecessor, DependencyType::FinishToFinish)],
        );
        let id = successor.id;

        let schedule = plan(vec![predecessor, successor]);

        assert_eq!(moves(&schedule), [(id, Some(17), Some(20))]);
        assert_eq!(schedule.out_of_sync, []);
    }

    #[test]
    fn plan_should_move_start_and_due_together_for_start_to_finish() {
        let predecessor = task(Some(10), Some(20));
        let successor = depending_on(
            task(Some(2), Some(5)),
            &[(&predecessor, DependencyType::StartToFinish)],
        );
        let id = successor.id;

        let schedule = plan(vec![predecessor, successor]);

        assert_eq!(moves(&schedule), [(id, Some(7), Some(10))]);
        assert_eq!(schedule.out_of_sync, []);
    }

    #[test]
    fn plan_should_preserve_span_when_no_duration() {
        let predecessor = task(Some(1), Some(10));
        let successor = finish_to_start(task(Some(3), Some(9)), &predecessor);
        let id = successor.id;

        let schedule = plan(vec![predecessor, successor]);

        assert_eq!(moves(&schedule), [(id, Some(10), Some(16))]);
    }

    #[test]
    fn plan_should_set_due_from_duration() {
        // The duration (2 days) replaces the six-day span the task had.
        let predecessor = task(Some(1), Some(10));
        let successor = finish_to_start(with_duration(task(Some(3), Some(9)), 2), &predecessor);
        let id = successor.id;

        let schedule = plan(vec![predecessor, successor]);

        assert_eq!(moves(&schedule), [(id, Some(10), Some(12))]);
    }

    #[test]
    fn plan_should_place_a_dateless_task_from_its_predecessor_and_duration() {
        let predecessor = task(Some(1), Some(10));
        let successor = finish_to_start(with_duration(task(None, None), 3), &predecessor);
        let milestone = finish_to_start(with_duration(task(None, None), 0), &predecessor);
        let (id, milestone_id) = (successor.id, milestone.id);

        let schedule = plan(vec![predecessor, successor, milestone]);

        assert_eq!(
            moves(&schedule),
            [(id, Some(10), Some(13)), (milestone_id, Some(10), Some(10))]
        );
    }

    #[test]
    fn plan_should_set_only_start_for_a_dateless_task_without_duration() {
        let predecessor = task(Some(1), Some(10));
        let successor = finish_to_start(task(None, None), &predecessor);
        let id = successor.id;

        let schedule = plan(vec![predecessor, successor]);

        assert_eq!(moves(&schedule), [(id, Some(10), None)]);
    }

    #[test]
    fn plan_should_move_only_the_one_date_a_task_has() {
        let predecessor = task(Some(1), Some(10));
        let start_only = finish_to_start(task(Some(3), None), &predecessor);
        let due_only = depending_on(
            task(None, Some(5)),
            &[(&predecessor, DependencyType::FinishToFinish)],
        );
        let (start_only_id, due_only_id) = (start_only.id, due_only.id);

        let schedule = plan(vec![predecessor, start_only, due_only]);

        assert_eq!(
            moves(&schedule),
            [
                (start_only_id, Some(10), None),
                (due_only_id, None, Some(10))
            ]
        );
    }

    #[test]
    fn plan_should_set_due_from_duration_for_a_task_with_only_a_start() {
        // Its start already respects the dependency, so nothing pushes it;
        // the missing due date is filled all the same.
        let predecessor = task(Some(1), Some(10));
        let successor = finish_to_start(with_duration(task(Some(12), None), 3), &predecessor);
        let milestone = finish_to_start(with_duration(task(Some(12), None), 0), &predecessor);
        let (id, milestone_id) = (successor.id, milestone.id);

        let schedule = plan(vec![predecessor, successor, milestone]);

        assert_eq!(
            moves(&schedule),
            [(id, Some(12), Some(15)), (milestone_id, Some(12), Some(12))]
        );
    }

    #[test]
    fn plan_should_set_start_from_duration_for_a_task_with_only_a_due() {
        let predecessor = task(Some(1), Some(10));
        let successor = depending_on(
            with_duration(task(None, Some(20)), 3),
            &[(&predecessor, DependencyType::FinishToFinish)],
        );
        let id = successor.id;

        let schedule = plan(vec![predecessor, successor]);

        assert_eq!(moves(&schedule), [(id, Some(17), Some(20))]);
    }

    #[test]
    fn plan_should_stop_at_the_first_date_when_a_duration_reaches_before_it() {
        let mut due_only = with_duration(task(None, None), u32::MAX);
        due_only.due_date = Some(date(20));

        let schedule = plan(vec![due_only]);

        assert_eq!(schedule.moved.len(), 1);
        assert_eq!(schedule.moved[0].after.start_date, Some(NaiveDate::MIN));
        assert_eq!(schedule.moved[0].after.due_date, Some(date(20)));
    }

    #[test]
    fn plan_should_fill_a_due_date_for_a_task_without_predecessors() {
        let start_only = with_duration(task(Some(5), None), 3);
        let due_only = with_duration(task(None, Some(20)), 4);
        let dateless = with_duration(task(None, None), 2);
        let without_duration = task(Some(5), None);
        let (start_only_id, due_only_id) = (start_only.id, due_only.id);

        let schedule = plan(vec![start_only, due_only, dateless, without_duration]);

        assert_eq!(
            moves(&schedule),
            [
                (start_only_id, Some(5), Some(8)),
                (due_only_id, Some(16), Some(20))
            ]
        );
    }

    #[test]
    fn plan_should_place_a_dateless_task_from_a_finish_anchor_and_its_duration() {
        let predecessor = task(Some(1), Some(10));
        let successor = depending_on(
            with_duration(task(None, None), 3),
            &[(&predecessor, DependencyType::FinishToFinish)],
        );
        let id = successor.id;

        let schedule = plan(vec![predecessor, successor]);

        assert_eq!(moves(&schedule), [(id, Some(7), Some(10))]);
    }

    #[test]
    fn plan_should_keep_its_duration_for_a_start_only_task_under_a_finish_anchor() {
        // The finish anchor (the 3rd) is before the start, so it asks for
        // nothing: the due date comes from the duration, not the anchor.
        let predecessor = task(Some(1), Some(3));
        let successor = depending_on(
            with_duration(task(Some(10), None), 5),
            &[(&predecessor, DependencyType::FinishToFinish)],
        );
        // Here the anchor (the 20th) is past start + duration, so the
        // whole task moves later and keeps its five days.
        let late = task(Some(1), Some(20));
        let pushed = depending_on(
            with_duration(task(Some(10), None), 5),
            &[(&late, DependencyType::FinishToFinish)],
        );
        let (id, pushed_id) = (successor.id, pushed.id);

        let schedule = plan(vec![predecessor, successor, late, pushed]);

        assert_eq!(
            moves(&schedule),
            [(id, Some(10), Some(15)), (pushed_id, Some(15), Some(20))]
        );
    }

    #[test]
    fn plan_should_anchor_successors_on_a_due_date_it_filled() {
        let first = with_duration(task(Some(5), None), 3);
        let second = finish_to_start(task(Some(1), Some(2)), &first);
        let (first_id, second_id) = (first.id, second.id);

        // Listed successor-first, so the order comes from the graph.
        let schedule = plan(vec![second, first]);

        assert_eq!(
            moves(&schedule),
            [(first_id, Some(5), Some(8)), (second_id, Some(8), Some(9))]
        );
        assert_eq!(schedule.out_of_sync, []);
    }

    #[test]
    fn plan_should_not_fill_a_date_on_a_fixed_task() {
        let start_only = fixed(with_duration(task(Some(5), None), 3));
        let due_only = fixed(with_duration(task(None, Some(20)), 3));
        let mut done = with_duration(task(Some(5), None), 3);
        done.status = TaskStatus::Complete;
        let gone = deleted(with_duration(task(Some(5), None), 3));
        // Nothing to anchor on: the fixed task it depends on has no due.
        let successor = finish_to_start(task(Some(1), Some(2)), &start_only);

        let schedule = plan(vec![start_only, due_only, done, gone, successor]);

        assert_eq!(schedule.moved, []);
        assert_eq!(schedule.out_of_sync, []);
    }

    #[test]
    fn plan_should_not_leave_due_before_start_for_a_task_it_moves() {
        // A start placed after the only date the task had, and a due date
        // placed before the start it already had: the due date lands on
        // the start.
        let predecessor = task(Some(1), Some(10));
        let due_only = finish_to_start(task(None, Some(5)), &predecessor);
        let start_only = depending_on(
            task(Some(15), None),
            &[(&predecessor, DependencyType::FinishToFinish)],
        );
        let (due_only_id, start_only_id) = (due_only.id, start_only.id);

        let schedule = plan(vec![predecessor, due_only, start_only]);

        assert_eq!(
            moves(&schedule),
            [
                (due_only_id, Some(10), Some(10)),
                (start_only_id, Some(15), Some(15))
            ]
        );
    }

    #[test]
    fn plan_should_not_move_a_fixed_task_and_should_report_it_out_of_sync() {
        let predecessor = task(Some(1), Some(10));
        let successor = finish_to_start(fixed(task(Some(3), Some(9))), &predecessor);
        let expected = Task {
            out_of_sync: true,
            ..successor.clone()
        };

        let schedule = plan(vec![predecessor, successor]);

        assert_eq!(schedule.moved, []);
        assert_eq!(schedule.out_of_sync, [expected]);
    }

    #[test]
    fn plan_should_schedule_successors_from_a_fixed_tasks_actual_dates() {
        // The fixed task would end on the 16th if it could move; its
        // successor is placed from the 9th, where it actually ends.
        let first = task(Some(1), Some(10));
        let pinned = finish_to_start(fixed(task(Some(3), Some(9))), &first);
        let last = finish_to_start(task(Some(4), Some(6)), &pinned);
        let (pinned_id, last_id) = (pinned.id, last.id);

        let schedule = plan(vec![first, pinned, last]);

        assert_eq!(moves(&schedule), [(last_id, Some(9), Some(11))]);
        assert_eq!(ids(&schedule.out_of_sync), [pinned_id]);
    }

    #[test]
    fn plan_should_cascade_down_a_chain() {
        let first = task(Some(1), Some(10));
        let second = finish_to_start(task(Some(5), Some(8)), &first);
        let third = finish_to_start(task(Some(6), Some(9)), &second);
        let (second_id, third_id) = (second.id, third.id);

        // Listed successor-first: the order of the list is not the order
        // tasks are placed in.
        let schedule = plan(vec![third, second, first]);

        assert_eq!(
            moves(&schedule),
            [
                (second_id, Some(10), Some(13)),
                (third_id, Some(13), Some(16))
            ]
        );
        assert_eq!(schedule.out_of_sync, []);
    }

    #[test]
    fn plan_should_take_the_latest_of_several_predecessors() {
        let early = task(Some(1), Some(10));
        let late = task(Some(1), Some(15));
        let earliest = task(Some(1), Some(4));
        let successor = depending_on(
            task(Some(3), Some(5)),
            &[
                (&early, DependencyType::FinishToStart),
                (&late, DependencyType::FinishToStart),
                (&earliest, DependencyType::FinishToStart),
            ],
        );
        let id = successor.id;

        let schedule = plan(vec![early, late, earliest, successor]);

        assert_eq!(moves(&schedule), [(id, Some(15), Some(17))]);
    }

    #[test]
    fn plan_should_apply_each_edge_by_its_own_type_for_one_predecessor() {
        let predecessor = task(Some(10), Some(20));
        let after_finish = finish_to_start(task(Some(5), Some(7)), &predecessor);
        let after_start = depending_on(
            task(Some(5), Some(7)),
            &[(&predecessor, DependencyType::StartToStart)],
        );
        let (after_finish_id, after_start_id) = (after_finish.id, after_start.id);

        let schedule = plan(vec![predecessor, after_finish, after_start]);

        assert_eq!(
            moves(&schedule),
            [
                (after_finish_id, Some(20), Some(22)),
                (after_start_id, Some(10), Some(12))
            ]
        );
    }

    #[test]
    fn plan_should_apply_start_and_finish_constraints_to_one_task() {
        // The start moves to the 10th (start-to-start), taking the due
        // date to the 12th; finish-to-finish then needs the 20th, so both
        // move eight days more.
        let predecessor = task(Some(10), Some(20));
        let successor = depending_on(
            task(Some(5), Some(7)),
            &[
                (&predecessor, DependencyType::StartToStart),
                (&predecessor, DependencyType::FinishToFinish),
            ],
        );
        let id = successor.id;

        let schedule = plan(vec![predecessor, successor]);

        assert_eq!(moves(&schedule), [(id, Some(18), Some(20))]);
        assert_eq!(schedule.out_of_sync, []);
    }

    #[test]
    fn plan_should_keep_slack_when_nothing_is_violated() {
        // The second successor's duration disagrees with its dates; a task
        // that is not moved keeps its dates whatever its duration says.
        let predecessor = task(Some(1), Some(10));
        let successor = finish_to_start(task(Some(15), Some(18)), &predecessor);
        let with_other_duration =
            finish_to_start(with_duration(task(Some(10), Some(18)), 2), &predecessor);

        let schedule = plan(vec![predecessor, successor, with_other_duration]);

        assert_eq!(schedule, Schedule::default());
    }

    #[test]
    fn plan_should_not_pull_a_task_earlier() {
        // Only the finish needs to move, and it moves later; the anchors
        // the start is already past do not draw anything back.
        let predecessor = task(Some(2), Some(20));
        let successor = depending_on(
            task(Some(12), Some(15)),
            &[
                (&predecessor, DependencyType::StartToStart),
                (&predecessor, DependencyType::FinishToFinish),
            ],
        );
        let far_later = finish_to_start(task(Some(28), Some(30)), &successor);
        let id = successor.id;

        let schedule = plan(vec![predecessor, successor, far_later]);

        assert_eq!(moves(&schedule), [(id, Some(17), Some(20))]);
    }

    #[test]
    fn plan_should_not_move_a_completed_task() {
        let predecessor = task(Some(1), Some(10));
        let mut done = finish_to_start(task(Some(3), Some(9)), &predecessor);
        done.status = TaskStatus::Complete;
        done.completed_at = Some(Utc::now());
        let done_id = done.id;
        let next = finish_to_start(task(Some(4), Some(6)), &done);
        let next_id = next.id;

        let schedule = plan(vec![predecessor, done, next]);

        assert_eq!(moves(&schedule), [(next_id, Some(9), Some(11))]);
        assert_eq!(ids(&schedule.out_of_sync), [done_id]);
    }

    #[test]
    fn plan_should_ignore_soft_deleted_tasks() {
        let predecessor = task(Some(1), Some(10));
        let deleted_predecessor = deleted(task(Some(1), Some(25)));
        // Would move if it were live.
        let deleted_successor = deleted(finish_to_start(task(Some(3), Some(9)), &predecessor));
        let deleted_fixed = deleted(finish_to_start(fixed(task(Some(3), Some(9))), &predecessor));
        let successor = finish_to_start(task(Some(12), Some(14)), &deleted_predecessor);

        let schedule = plan(vec![
            predecessor,
            deleted_predecessor,
            deleted_successor,
            deleted_fixed,
            successor,
        ]);

        assert_eq!(schedule, Schedule::default());
    }

    #[test]
    fn plan_should_leave_a_dateless_task_without_predecessors_alone() {
        let dateless = task(None, None);
        let with_length = with_duration(task(None, None), 3);
        // Its predecessor has no date to place it from.
        let undated_predecessor = task(None, None);
        let successor = finish_to_start(with_duration(task(None, None), 3), &undated_predecessor);

        let schedule = plan(vec![dateless, with_length, undated_predecessor, successor]);

        assert_eq!(schedule, Schedule::default());
    }

    #[test]
    fn plan_should_flag_before_and_after_from_their_own_dates() {
        let predecessor = task(Some(1), Some(10));
        let successor = finish_to_start(task(Some(3), Some(9)), &predecessor);
        let before = Task {
            out_of_sync: true,
            ..successor.clone()
        };
        let after = Task {
            start_date: Some(date(10)),
            due_date: Some(date(16)),
            ..successor.clone()
        };

        let schedule = plan(vec![predecessor, successor]);

        assert_eq!(schedule.moved, [Rescheduled { before, after }]);
    }

    #[test]
    fn plan_should_move_nothing_when_run_on_its_own_result() {
        let first = task(Some(1), Some(10));
        let pinned = finish_to_start(fixed(task(Some(3), Some(9))), &first);
        let second = finish_to_start(with_duration(task(Some(5), Some(8)), 4), &first);
        let third = depending_on(
            task(Some(6), Some(9)),
            &[
                (&second, DependencyType::StartToStart),
                (&first, DependencyType::FinishToFinish),
                (&pinned, DependencyType::StartToFinish),
            ],
        );
        let dateless = finish_to_start(task(None, None), &third);
        let due_only = finish_to_start(task(None, Some(2)), &third);
        // Each of these two only has a date filled from its duration.
        let filled_due = with_duration(task(Some(5), None), 3);
        let filled_start = depending_on(
            with_duration(task(None, None), 3),
            &[(&first, DependencyType::FinishToFinish)],
        );
        let pinned_id = pinned.id;
        let tasks = vec![
            first,
            pinned,
            second,
            third,
            dateless,
            due_only,
            filled_due,
            filled_start,
        ];

        let schedule = plan(tasks.clone());
        let again = plan(applied(tasks, &schedule));

        assert_eq!(schedule.moved.len(), 6);
        assert_eq!(again.moved, []);
        assert_eq!(ids(&again.out_of_sync), [pinned_id]);
        assert_eq!(again.out_of_sync, schedule.out_of_sync);
    }

    #[test]
    fn plan_should_handle_a_chain_thousands_deep() {
        const LENGTH: usize = 5_000;
        // Every task runs 1..2 March and starts when the one before it is
        // due, so each is pushed one day further than the last.
        let mut chain = vec![task(Some(1), Some(2))];
        for _ in 1..LENGTH {
            let next = finish_to_start(task(Some(1), Some(2)), chain.last().unwrap());
            chain.push(next);
        }
        let last = chain.last().unwrap().id;
        // Listed last-first, so no task is placed in list order.
        chain.reverse();

        let schedule = plan(chain);

        assert_eq!(schedule.moved.len(), LENGTH - 1);
        assert_eq!(schedule.out_of_sync, []);
        let end = schedule.moved.last().unwrap();
        assert_eq!(end.after.id, last);
        let days = Days::new(LENGTH as u64 - 1);
        assert_eq!(end.after.start_date, date(1).checked_add_days(days));
        assert_eq!(end.after.due_date, date(2).checked_add_days(days));
    }

    #[test]
    fn plan_should_place_nothing_on_a_dependency_cycle() {
        // Every date here breaks a dependency, and none can be placed
        // after its predecessors: the two on the cycle and the one
        // downstream of it stay put and are reported.
        let mut one = task(Some(1), Some(10));
        let two = finish_to_start(task(Some(1), Some(10)), &one);
        one = finish_to_start(one, &two);
        let downstream = finish_to_start(task(Some(1), Some(10)), &two);
        let expected = [one.id, two.id, downstream.id];

        let schedule = plan(vec![one, two, downstream]);

        assert_eq!(schedule.moved, []);
        assert_eq!(ids(&schedule.out_of_sync), expected);
    }

    #[test]
    fn check_new_dependency_should_accept_two_distinct_tasks() {
        let store = InMemoryStore::default();
        let task = TaskId::new();
        let predecessor = TaskId::new();

        let result = store
            .transaction(|tx| Ok(check_new_dependency(tx, task, predecessor)))
            .unwrap();

        assert!(result.is_ok());
    }
}
