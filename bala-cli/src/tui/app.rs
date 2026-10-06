//! Selection-state and edit-mode model for the TUI's task list view.
//!
//! `App` tracks the current row selection over an in-memory list of
//! `TaskRow`s, plus the current interaction `Mode` and any inline error to
//! show the user. `apply_action` is the single place that mutates `App` and
//! (for actions that need it) calls through to `Core`; `handle_key` is a
//! thin wrapper around `keymap::key_to_action` + `apply_action` for the
//! real crossterm-backed event loop in `tui::mod::run`.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;

use bala_core::{
    Core, CoreError, DeleteMode, DependencyType, Field, NewTask, SiblingOrder, Store, Task, TaskId,
    TaskPatch, TaskStatus, UserId,
};
use crossterm::event::KeyEvent;

use crate::render::{self, TaskRow};
use crate::tui::keymap::{Action, key_to_action};
use crate::tui::mode::{DetailField, EditableField, Mode, Pane, PendingAction};

/// One task related to the selected row by a dependency, as shown in the
/// Detail pane's "Blocked by" list (a task it depends on) and "Blocks" list
/// (a task that depends on it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelatedTask {
    /// The related task's title.
    pub title: String,
    /// The related task's current status.
    pub status: TaskStatus,
}

/// Which tasks the list shows with respect to dependency blocking.
///
/// Blockedness comes from `Task::blocked_by` (a task with a live incomplete
/// predecessor), so it is the same whichever view is active; the view only
/// chooses which tasks are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlockedView {
    /// Every task.
    #[default]
    All,
    /// Only tasks waiting on an incomplete predecessor.
    Blocked,
    /// Only incomplete tasks that are not blocked.
    Ready,
}

impl BlockedView {
    /// The view after this one in the cycle `All -> Blocked -> Ready -> All`.
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Blocked,
            Self::Blocked => Self::Ready,
            Self::Ready => Self::All,
        }
    }

    /// Lowercase name shown in the status line.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Blocked => "blocked",
            Self::Ready => "ready",
        }
    }

    /// The view named by `label` (the inverse of [`BlockedView::label`]),
    /// or `None` when `label` names no view.
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        [Self::All, Self::Blocked, Self::Ready]
            .into_iter()
            .find(|view| view.label() == label)
    }

    /// Whether `task` is listed under this view.
    fn includes(self, task: &Task) -> bool {
        match self {
            Self::All => true,
            Self::Blocked => !task.blocked_by.is_empty(),
            Self::Ready => task.status == TaskStatus::Incomplete && task.blocked_by.is_empty(),
        }
    }
}

/// Selection state over an in-memory list of task rows, plus the current
/// interaction mode and any inline error to display.
///
/// Selection-boundary behavior is clamp, not wrap: `move_up` at the first
/// row and `move_down` at the last row both leave the selection unchanged.
pub struct App {
    rows: Vec<TaskRow>,
    selected: Option<usize>,
    mode: Mode,
    pane: Pane,
    detail_field: DetailField,
    error: Option<String>,
    type_labels: HashMap<String, String>,
    user_names: HashMap<UserId, String>,
    descriptions: HashMap<TaskId, Option<String>>,
    /// The full cached fetch of every task (from the most recent
    /// `Core::get_tree` call), independent of collapse state. `rebuild_rows`
    /// re-derives `rows` from this plus `collapsed` whenever collapse state
    /// changes, without needing to re-fetch from `Core`.
    tasks: Vec<Task>,
    /// The type filter `tasks` was actually fetched with (`None` = the
    /// unfiltered tree). Normally equal to `type_filter`, but the two part
    /// when `type_filter` changes and the refetch that should follow fails,
    /// leaving `tasks` as it was; `unfiltered_tree` goes by this one.
    tasks_filter: Option<String>,
    /// Sibling order fetched alongside `tasks` (`Core::sibling_order`);
    /// kept so a later reorder/reparent can re-render without a reload.
    sibling_order: SiblingOrder,
    /// Ids of tasks whose children are currently hidden from `rows`.
    collapsed: HashSet<TaskId>,
    /// Whether a single `d` key press is pending a second consecutive `d` to
    /// complete the `dd` delete-confirm sequence. Reset to `false` by every
    /// action other than `DKeyPressed` itself, so `d`, some unrelated
    /// action, `d` doesn't count as two consecutive presses.
    pending_d: bool,
    /// The active type filter applied by `refresh_rows_from_tree`, or `None`
    /// for no filtering. Cycled by `Action::CycleTypeFilter` (`f`) through
    /// `available_type_keys`.
    type_filter: Option<String>,
    /// Every configured task type's key, in the order `f` cycles through
    /// them. Populated once at startup by `tui::mod::run` (via
    /// [`App::with_type_filter_state`]) from `Core::list_task_types()`.
    available_type_keys: Vec<String>,
    /// The selected row's live predecessors, complete or not, in
    /// `depends_on` order. Loaded with `Core::get_task` per id by
    /// `reload_predecessors`, so predecessors the type filter hides from
    /// `rows` are still listed. Empty whenever the List pane is showing.
    predecessors: Vec<RelatedTask>,
    /// The live tasks that depend on the selected row, in tree order.
    /// Loaded by `reload_dependents` from an unfiltered tree, so dependents
    /// the type filter hides from `rows` are still listed. Empty whenever
    /// the List pane is showing.
    dependents: Vec<RelatedTask>,
    /// The active blocked/ready view, cycled by `Action::CycleBlockedView`
    /// (`b`). Applied together with the type filter whenever `rows` are
    /// derived from `tasks`.
    blocked_view: BlockedView,
}

impl App {
    /// Builds a new `App` over `rows`, selecting the first row when present,
    /// starting in `Mode::Normal` with no error and empty lookup maps.
    ///
    /// Use [`App::with_lookup_maps`] to attach the `type_labels`/
    /// `user_names` maps `apply_action` needs to project a newly created
    /// task into a displayable row.
    #[must_use]
    pub fn new(rows: Vec<TaskRow>) -> Self {
        let selected = if rows.is_empty() { None } else { Some(0) };
        Self {
            rows,
            selected,
            mode: Mode::Normal,
            pane: Pane::List,
            detail_field: DetailField::Title,
            error: None,
            type_labels: HashMap::new(),
            user_names: HashMap::new(),
            descriptions: HashMap::new(),
            tasks: Vec::new(),
            tasks_filter: None,
            sibling_order: SiblingOrder::default(),
            collapsed: HashSet::new(),
            pending_d: false,
            type_filter: None,
            available_type_keys: Vec::new(),
            predecessors: Vec::new(),
            dependents: Vec::new(),
            blocked_view: BlockedView::default(),
        }
    }

    /// Attaches the cached full task fetch used by [`App::rebuild_rows`] to
    /// re-derive `rows` after a collapse/expand action, without needing to
    /// re-fetch from `Core`. Builder-style for the same reason as
    /// [`App::with_lookup_maps`]. The tasks are taken to be the unfiltered
    /// tree unless [`App::with_type_filter_state`] says which type filter
    /// they were fetched with.
    #[must_use]
    pub fn with_tasks(mut self, tasks: Vec<Task>) -> Self {
        self.tasks = tasks;
        self
    }

    /// Attaches the fetched sibling order used by [`App::rebuild_rows`], so a
    /// later reorder/reparent can re-render without a reload. Call before
    /// [`App::with_collapsed`].
    #[must_use]
    pub fn with_sibling_order(mut self, sibling_order: SiblingOrder) -> Self {
        self.sibling_order = sibling_order;
        self
    }

    /// Sets the initial collapse state and re-derives `rows` from it,
    /// letting `tui::mod::run` restore a persisted `ViewState.collapsed` at
    /// startup. Must be called after [`App::with_tasks`] in the builder
    /// chain — `rebuild_rows` reads `self.tasks`, so calling this first would
    /// rebuild against an empty task list. Builder-style for the same reason
    /// as [`App::with_lookup_maps`].
    #[must_use]
    pub fn with_collapsed(mut self, collapsed: HashSet<TaskId>) -> Self {
        self.collapsed = collapsed;
        self.rebuild_rows();
        self
    }

    /// Attaches the type-label/user-name lookup maps used to project a
    /// newly created or edited task into a `TaskRow`. Builder-style so
    /// `tui::mod::run` can set them once at startup without changing
    /// `App::new`'s signature (and every existing test/call site with it).
    #[must_use]
    pub fn with_lookup_maps(
        mut self,
        type_labels: HashMap<String, String>,
        user_names: HashMap<UserId, String>,
    ) -> Self {
        self.type_labels = type_labels;
        self.user_names = user_names;
        self
    }

    /// Attaches the per-task description lookup the Detail pane uses to
    /// show/edit a task's description (`TaskRow` itself carries no
    /// description — only title/type/status/assignee — since the list view
    /// doesn't show one). Builder-style for the same
    /// reason as [`App::with_lookup_maps`].
    #[must_use]
    pub fn with_descriptions(mut self, descriptions: HashMap<TaskId, Option<String>>) -> Self {
        self.descriptions = descriptions;
        self
    }

    /// Sets the active type filter and the ordered list of type keys `f`
    /// cycles through, letting `tui::mod::run` restore a persisted
    /// `ViewState.filter_type_key` and seed `available_type_keys` from
    /// `Core::list_task_types()` at startup. Unlike [`App::with_collapsed`],
    /// this doesn't rebuild `rows` itself: the initial `rows` passed to
    /// [`App::new`] are expected to already reflect `type_filter` (via a
    /// filtered initial `Core::get_tree` fetch in `tui::mod::run`) — a
    /// restored filter only changes what `refresh_rows_from_tree` fetches
    /// going forward. For the same reason `type_filter` is recorded as the
    /// filter the cached tasks were fetched with.
    #[must_use]
    pub fn with_type_filter_state(
        mut self,
        type_filter: Option<String>,
        available_type_keys: Vec<String>,
    ) -> Self {
        self.tasks_filter.clone_from(&type_filter);
        self.type_filter = type_filter;
        self.available_type_keys = available_type_keys;
        self
    }

    /// Sets the active blocked/ready view and re-derives `rows` from it,
    /// letting `tui::mod::run` restore a persisted view at startup. Call
    /// after [`App::with_tasks`], since the rows are rebuilt from the cached
    /// tasks.
    #[must_use]
    pub fn with_blocked_view(mut self, blocked_view: BlockedView) -> Self {
        self.blocked_view = blocked_view;
        self.rebuild_rows();
        self
    }

    /// Moves the selection down by one row, clamping at the last row.
    /// No-op when there are no rows.
    pub fn move_down(&mut self) {
        let Some(selected) = self.selected else {
            return;
        };
        self.selected = Some((selected + 1).min(self.rows.len() - 1));
    }

    /// Moves the selection up by one row, clamping at the first row.
    /// No-op when there are no rows.
    pub fn move_up(&mut self) {
        let Some(selected) = self.selected else {
            return;
        };
        self.selected = Some(selected.saturating_sub(1));
    }

    /// Returns the currently selected row, or `None` when there are no rows.
    #[must_use]
    pub fn selected_row(&self) -> Option<&TaskRow> {
        self.selected.and_then(|index| self.rows.get(index))
    }

    /// Returns all rows, in display order.
    #[must_use]
    pub fn rows(&self) -> &[TaskRow] {
        &self.rows
    }

    /// Returns the index of the currently selected row, or `None` when
    /// there are no rows.
    #[must_use]
    pub fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    /// Returns the current interaction mode.
    #[must_use]
    pub fn mode(&self) -> &Mode {
        &self.mode
    }

    /// Returns the current inline error message, if any.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Returns the set of task ids currently collapsed, for
    /// `tui::mod::run` to persist into `ViewState` on quit.
    #[must_use]
    pub fn collapsed(&self) -> &HashSet<TaskId> {
        &self.collapsed
    }

    /// Returns the active blocked/ready view.
    #[must_use]
    pub fn blocked_view(&self) -> BlockedView {
        self.blocked_view
    }

    /// Returns the active type filter, for `tui::mod::run` to persist into
    /// `ViewState.filter_type_key` on quit.
    #[must_use]
    pub fn type_filter(&self) -> Option<&str> {
        self.type_filter.as_deref()
    }

    /// Returns the currently focused pane.
    #[must_use]
    pub fn pane(&self) -> Pane {
        self.pane
    }

    /// Returns the field currently under the cursor in the Detail pane.
    #[must_use]
    pub fn detail_field(&self) -> DetailField {
        self.detail_field
    }

    /// Returns the selected row's live predecessors, complete or not
    /// (empty when it depends on nothing, nothing is selected, or the List
    /// pane is showing).
    #[must_use]
    pub fn predecessors(&self) -> &[RelatedTask] {
        &self.predecessors
    }

    /// Returns the live tasks that depend on the selected row (empty when
    /// nothing depends on it, nothing is selected, or the List pane is
    /// showing).
    #[must_use]
    pub fn dependents(&self) -> &[RelatedTask] {
        &self.dependents
    }

    /// Returns the description of the task with `id`, or `None` when it has
    /// none (or `id` isn't in the lookup, which shouldn't happen once
    /// `tui::mod::run` threads descriptions for every fetched task).
    #[must_use]
    pub fn description_of(&self, id: TaskId) -> Option<&str> {
        self.descriptions.get(&id)?.as_deref()
    }

    /// Selects the row whose id matches `id`, restoring a previously
    /// persisted selection (see `tui::mod::run`, which loads a `ViewState`
    /// and calls this right after constructing `App`).
    ///
    /// Leaves the current selection completely untouched when `id` is
    /// `None`, or when it's `Some` but no row matches (e.g. the persisted
    /// task was since deleted) — in either case `App::new`'s own default
    /// (first row, if any) stands.
    pub fn select_by_id(&mut self, id: Option<TaskId>) {
        let Some(id) = id else {
            return;
        };
        if let Some(index) = self.rows.iter().position(|row| row.id == id) {
            self.selected = Some(index);
        }
    }

    /// Projects the cached `tasks` into rows, keeping only the tasks the
    /// active [`BlockedView`] lists. A task whose parent is filtered out
    /// renders at top level. Blockedness is read from each task's own
    /// `blocked_by`, so it does not depend on which tasks are listed.
    fn build_rows(&self) -> Vec<TaskRow> {
        let visible: Vec<Task>;
        let tasks = if self.blocked_view == BlockedView::All {
            &self.tasks
        } else {
            visible = self
                .tasks
                .iter()
                .filter(|task| self.blocked_view.includes(task))
                .cloned()
                .collect();
            &visible
        };
        render::task_rows(
            tasks,
            &self.sibling_order,
            &self.type_labels,
            &self.user_names,
            &self.collapsed,
        )
    }

    /// Re-derives `rows` from `tasks`/`type_labels`/`user_names`/`collapsed`,
    /// reselecting the row that was focused before the rebuild so a
    /// collapse/expand never loses the user's place. `CollapseFocused`/
    /// `ExpandFocused` only ever act on the focused row itself, so its id
    /// always survives those two rebuilds. `CollapseAll`, however, can
    /// collapse an *ancestor* of the focused row too, hiding the focused
    /// row itself — in that case `select_id_or_nearest_visible_ancestor`
    /// walks up `parent_id` to reselect the nearest still-visible ancestor
    /// instead of leaving `self.selected` as a stale index into the old
    /// (differently-shaped) `rows`.
    fn rebuild_rows(&mut self) {
        let selected_id = self.selected_row().map(|row| row.id);
        self.rows = self.build_rows();
        if let Some(id) = selected_id {
            self.select_id_or_nearest_visible_ancestor(id);
        }
    }

    /// Selects the row for `id` if it's present in `self.rows`; otherwise
    /// walks up `id`'s `parent_id` (in `self.tasks`) looking for the
    /// nearest ancestor that IS present, and selects that instead — an
    /// ancestor only disappears from `rows` by being collapsed, and a
    /// collapsed task's own row is always still rendered, so this walk
    /// terminates at the latest by a top-level root, which `task_rows`
    /// never hides. If somehow no ancestor is present either (defensive:
    /// should be unreachable given that guarantee), clamps `self.selected`
    /// to a valid index into the new `rows` instead of leaving it stale.
    fn select_id_or_nearest_visible_ancestor(&mut self, mut id: TaskId) {
        loop {
            if let Some(index) = self.rows.iter().position(|row| row.id == id) {
                self.selected = Some(index);
                return;
            }
            let Some(task) = self.tasks.iter().find(|task| task.id == id) else {
                break;
            };
            let Some(parent_id) = task.parent_id else {
                break;
            };
            id = parent_id;
        }
        self.selected = if self.rows.is_empty() {
            None
        } else {
            Some(self.selected.unwrap_or(0).min(self.rows.len() - 1))
        };
    }
}

/// Dispatches one key event against `app`, delegating to `key_to_action`
/// (mode-aware key mapping) and `apply_action` (state mutation, including
/// any `Core` call the resulting action needs).
pub fn handle_key<S: Store>(app: &mut App, core: &mut Core<S>, key: KeyEvent) -> ControlFlow<()> {
    let action = key_to_action(app.mode(), app.pane(), app.detail_field(), key);
    apply_action(app, core, action)
}

/// Applies one `Action` to `app`, calling through to `core` for actions that
/// need it (`SubmitInsert`).
///
/// `MoveDown`/`MoveUp` move the list selection; `Quit` signals quit via
/// `ControlFlow::Break`; `StartInsertNewTitle` enters `Mode::Insert` with an
/// empty buffer; `InsertChar`/`Backspace` edit the buffer; `CancelInsert`
/// discards the buffer and returns to `Mode::Normal` (leaving `app.pane`
/// untouched) without calling `Core`; `SubmitInsert` calls `Core::create_task`
/// (for `NewTitle`) or `Core::update_task` (for `Title`/`Description`) — on
/// success `app` returns to `Mode::Normal`, on failure (e.g. an empty title)
/// `app.error` is set to the error's message and `app` stays in
/// `Mode::Insert` with the buffer unchanged, so the user can fix and
/// resubmit. `EnterDetail`/`LeaveDetail` switch `app.pane` between `List`
/// and `Detail` (defaulting the cursor to `Title` on entry); `DetailCursorDown`/
/// `DetailCursorUp` move `app.detail_field`; `StartEditTitle`/
/// `StartEditDescription` enter `Mode::Insert` prefilled with the selected
/// task's current title/description (empty string when there's no
/// description). `StartReparent` enters `Mode::Insert` scoped to
/// `EditableField::Parent`, prefilled with the selected task's current
/// parent id (fetched fresh via `Core::get_task`), and `SubmitInsert` for
/// that field calls `Core::set_parent`. `StartSetType` enters `Mode::Insert`
/// scoped to `EditableField::TypeKey`, prefilled with the selected task's
/// current `type_key` (fetched fresh via `Core::get_task`), and
/// `SubmitInsert` for that field calls `Core::update_task` with a
/// `TaskPatch { type_key: Field::Set(...), .. }`, surfacing
/// `CoreError::UnknownTaskType` inline like other validation errors when the
/// typed key names no configured type. `StartAddDependency` enters
/// `Mode::Insert` scoped to `EditableField::AddPredecessor` with an empty
/// buffer, and `SubmitInsert` for that field calls `Core::add_dependency`
/// with the one task id typed and `DependencyType::FinishToStart`, after
/// checking the task does not already depend on it.
/// `StartRemoveDependency` enters `Mode::Insert` scoped to
/// `EditableField::RemovePredecessor`, prefilled with the selected task's
/// predecessor id when it has exactly one (fetched fresh via
/// `Core::get_task`), and `SubmitInsert` for that field calls
/// `Core::remove_dependency` with the one task id typed, after checking the
/// task really depends on it.
/// `OpenHelp` enters `Mode::Help`,
/// remembering the current mode as `previous`; `CloseHelp` restores
/// `previous` if `app` is currently in `Mode::Help`, otherwise it does
/// nothing. `CycleTypeFilter` advances `app.type_filter` through `None ->
/// available_type_keys[0] -> ... -> None` (see `cycle_type_filter`) and then
/// calls `refresh_rows_from_tree` so the visible rows immediately reflect
/// the new filter. `ConfirmDelete(mode)` deletes the task pending in a
/// `Mode::Confirm` for `PendingAction::Delete`/`DeleteWithChildren` with
/// `mode` (see `confirm_delete`); in any other mode it does nothing. `Noop`
/// does nothing.
///
/// If the Detail pane was showing a task and the action took that task out
/// of `rows` (an edit that moves it out of the active blocked/ready view or
/// type filter), `app.pane` falls back to `Pane::List` with whatever
/// selection the refresh chose, so the pane never silently shows a
/// different task or nothing. A task still in `rows` keeps its Detail pane.
///
/// After every action except `InsertChar`/`Backspace` the selected row's
/// predecessors and dependents are reloaded (see `reload_detail_lists`), so
/// every selection change, pane change and tree refresh is covered without
/// each call site repeating it. They are read from the store only while the
/// Detail pane is showing and cleared otherwise. The two keystroke actions
/// only edit the buffer, so they skip the reload.
pub fn apply_action<S: Store>(
    app: &mut App,
    core: &mut Core<S>,
    action: Action,
) -> ControlFlow<()> {
    let edits_buffer_only = matches!(action, Action::InsertChar(_) | Action::Backspace);
    let shown_in_detail = if app.pane == Pane::Detail {
        app.selected_row().map(|row| row.id)
    } else {
        None
    };
    let flow = dispatch_action(app, core, action);
    if let Some(id) = shown_in_detail
        && !app.rows.iter().any(|row| row.id == id)
    {
        app.pane = Pane::List;
    }
    if !edits_buffer_only {
        reload_detail_lists(app, core);
    }
    flow
}

/// Reloads the two lists the Detail pane shows for the selected row: its
/// predecessors (see `reload_predecessors`) and its dependents (see
/// `reload_dependents`). Both are only loaded while the Detail pane is
/// showing; in the List pane, where neither is drawn, they are just cleared.
pub fn reload_detail_lists<S: Store>(app: &mut App, core: &Core<S>) {
    reload_predecessors(app, core);
    reload_dependents(app, core);
}

/// Reloads `app.predecessors` for the selected row: one `Core::get_task`
/// per entry in the task's `depends_on`, in that order, so a predecessor
/// stays listed once it is complete and one hidden from `rows` by the type
/// filter is still listed. A soft-deleted predecessor reads as `None` and
/// is skipped. The list is left empty, without reading anything, while the
/// List pane is showing. A backend failure sets `app.error` (unless an error
/// is already shown) and leaves the list empty.
fn reload_predecessors<S: Store>(app: &mut App, core: &Core<S>) {
    app.predecessors.clear();
    if app.pane != Pane::Detail {
        return;
    }
    let Some(id) = app.selected_row().map(|row| row.id) else {
        return;
    };
    let Some(task) = app.tasks.iter().find(|task| task.id == id) else {
        return;
    };
    let predecessor_ids: Vec<TaskId> = task
        .depends_on
        .iter()
        .map(|dependency| dependency.predecessor_id)
        .collect();
    for predecessor_id in predecessor_ids {
        match core.get_task(predecessor_id) {
            Ok(Some(predecessor)) => app.predecessors.push(RelatedTask {
                title: predecessor.title,
                status: predecessor.status,
            }),
            Ok(None) => {}
            Err(err) => {
                if app.error.is_none() {
                    app.error = Some(err.to_string());
                }
                app.predecessors.clear();
                return;
            }
        }
    }
}

/// Reloads `app.dependents` for the selected row: the tasks whose
/// `depends_on` names it, found with `render::dependents_of` over the
/// unfiltered tree (see `unfiltered_tree`), so a dependent hidden from
/// `rows` by the type filter is still listed; a soft-deleted dependent is
/// not in the tree and so is not listed. The list is left empty, without
/// scanning or fetching anything, while the List pane is showing. A backend
/// failure sets `app.error` (unless an error is already shown) and leaves
/// the list empty.
fn reload_dependents<S: Store>(app: &mut App, core: &Core<S>) {
    app.dependents.clear();
    if app.pane != Pane::Detail {
        return;
    }
    let Some(id) = app.selected_row().map(|row| row.id) else {
        return;
    };
    let dependents = match unfiltered_tree(app, core) {
        Ok(tasks) => render::dependents_of(&[id], &tasks)
            .into_iter()
            .map(|dependent| RelatedTask {
                title: dependent.title.clone(),
                status: dependent.status,
            })
            .collect(),
        Err(err) => {
            if app.error.is_none() {
                app.error = Some(err.to_string());
            }
            return;
        }
    };
    app.dependents = dependents;
}

/// Returns every live task regardless of `app.type_filter`: the cached
/// `app.tasks` when it was fetched without a type filter (it already is the
/// unfiltered tree), otherwise one `Core::get_tree` with the default filter.
/// Which of the two applies is read from `app.tasks_filter`, not
/// `app.type_filter`: after a failed refetch the cache can still hold the
/// tree of the previous filter.
///
/// # Errors
///
/// Returns the `CoreError` from `Core::get_tree` when the cache is filtered
/// and the fetch fails.
fn unfiltered_tree<'a, S: Store>(
    app: &'a App,
    core: &Core<S>,
) -> Result<Cow<'a, [Task]>, CoreError> {
    if app.tasks_filter.is_none() {
        return Ok(Cow::Borrowed(&app.tasks));
    }
    core.get_tree(bala_core::TreeFilter::default())
        .map(Cow::Owned)
}

#[allow(clippy::too_many_lines)] // one big dispatch table by design; see `apply_action`
fn dispatch_action<S: Store>(app: &mut App, core: &mut Core<S>, action: Action) -> ControlFlow<()> {
    // Every action other than `DKeyPressed` itself resets `pending_d`, so a
    // `d`, some unrelated action, `d` sequence doesn't count as two
    // consecutive presses. `was_pending_d` captures the value as it stood
    // before this reset, for `DKeyPressed`'s own arm to consult.
    let was_pending_d = app.pending_d;
    app.pending_d = false;

    match action {
        Action::MoveDown => {
            app.move_down();
            ControlFlow::Continue(())
        }
        Action::MoveUp => {
            app.move_up();
            ControlFlow::Continue(())
        }
        Action::Quit => ControlFlow::Break(()),
        Action::StartInsertNewTitle => {
            start_insert_new_title(app);
            ControlFlow::Continue(())
        }
        Action::StartInsertNewSubtask => {
            start_insert_new_subtask(app);
            ControlFlow::Continue(())
        }
        Action::StartReparent => {
            start_reparent(app, core);
            ControlFlow::Continue(())
        }
        Action::StartSetType => {
            start_set_type(app, core);
            ControlFlow::Continue(())
        }
        Action::StartAddDependency => {
            start_add_dependency(app);
            ControlFlow::Continue(())
        }
        Action::StartRemoveDependency => {
            start_remove_dependency(app, core);
            ControlFlow::Continue(())
        }
        Action::InsertChar(c) => {
            insert_char(app, c);
            ControlFlow::Continue(())
        }
        Action::Backspace => {
            backspace(app);
            ControlFlow::Continue(())
        }
        Action::CancelInsert => {
            app.mode = Mode::Normal;
            app.error = None;
            ControlFlow::Continue(())
        }
        Action::SubmitInsert => {
            submit_insert(app, core);
            ControlFlow::Continue(())
        }
        Action::EnterDetail => {
            enter_detail(app);
            ControlFlow::Continue(())
        }
        Action::LeaveDetail => {
            app.pane = Pane::List;
            app.error = None;
            ControlFlow::Continue(())
        }
        Action::DetailCursorDown => {
            app.detail_field = DetailField::Description;
            ControlFlow::Continue(())
        }
        Action::DetailCursorUp => {
            app.detail_field = DetailField::Title;
            ControlFlow::Continue(())
        }
        Action::StartEditTitle => {
            start_edit_title(app);
            ControlFlow::Continue(())
        }
        Action::StartEditDescription => {
            start_edit_description(app);
            ControlFlow::Continue(())
        }
        Action::DKeyPressed => {
            handle_d_key_pressed(app, core, was_pending_d);
            ControlFlow::Continue(())
        }
        Action::ConfirmYes => {
            confirm_yes(app, core);
            ControlFlow::Continue(())
        }
        Action::ConfirmDelete(mode) => {
            if let Mode::Confirm {
                action: PendingAction::Delete(id) | PendingAction::DeleteWithChildren(id),
                ..
            } = app.mode
            {
                confirm_delete(app, core, id, mode);
            }
            ControlFlow::Continue(())
        }
        Action::ConfirmNo => {
            app.mode = Mode::Normal;
            ControlFlow::Continue(())
        }
        Action::ToggleComplete => {
            handle_toggle_complete(app, core);
            ControlFlow::Continue(())
        }
        Action::OpenHelp => {
            app.mode = Mode::Help {
                previous: Box::new(app.mode.clone()),
            };
            ControlFlow::Continue(())
        }
        Action::CloseHelp => {
            if let Mode::Help { previous } = &app.mode {
                app.mode = (**previous).clone();
            }
            ControlFlow::Continue(())
        }
        Action::CollapseFocused => {
            collapse_focused(app);
            ControlFlow::Continue(())
        }
        Action::ExpandFocused => {
            expand_focused(app);
            ControlFlow::Continue(())
        }
        Action::ExpandAll => {
            expand_all(app);
            ControlFlow::Continue(())
        }
        Action::CollapseAll => {
            collapse_all(app);
            ControlFlow::Continue(())
        }
        Action::CycleTypeFilter => {
            cycle_type_filter(app);
            refresh_rows_from_tree(app, core, app.selected_row().map(|row| row.id));
            ControlFlow::Continue(())
        }
        Action::CycleBlockedView => {
            app.blocked_view = app.blocked_view.next();
            let selected = app.selected_row().map(|row| row.id);
            refresh_rows_from_tree(app, core, selected);
            ControlFlow::Continue(())
        }
        Action::MoveTaskDown => {
            move_focused_task(app, core, bala_core::Direction::Down);
            ControlFlow::Continue(())
        }
        Action::MoveTaskUp => {
            move_focused_task(app, core, bala_core::Direction::Up);
            ControlFlow::Continue(())
        }
        Action::IndentTask => {
            reparent_focused_task(app, core, Reparent::Indent);
            ControlFlow::Continue(())
        }
        Action::OutdentTask => {
            reparent_focused_task(app, core, Reparent::Outdent);
            ControlFlow::Continue(())
        }
        Action::Noop => ControlFlow::Continue(()),
    }
}

/// Handles `Action::MoveTaskDown`/`MoveTaskUp`: swaps the focused task with
/// its neighbor among its siblings, via `Core::move_sibling`. The swap uses
/// the full sibling order, so with a type filter active the neighbor may be
/// hidden and a press can look like a no-op. On success the tree is
/// refetched and selection stays on the moved task. At either end of the
/// sibling list nothing changes and no error is shown; Core errors are
/// surfaced via `app.error`.
fn move_focused_task<S: Store>(app: &mut App, core: &mut Core<S>, direction: bala_core::Direction) {
    let Some(id) = app.selected_row().map(|row| row.id) else {
        return;
    };
    match core.move_sibling(id, direction) {
        Ok(false) => {}
        Ok(true) => {
            app.error = None;
            refresh_rows_from_tree(app, core, Some(id));
        }
        Err(err) => app.error = Some(err.to_string()),
    }
}

/// Which way `reparent_focused_task` moves the focused task.
#[derive(Clone, Copy)]
enum Reparent {
    Indent,
    Outdent,
}

/// Handles `Action::IndentTask`/`OutdentTask` via `Core::indent_task` /
/// `Core::outdent_task`, which read the task's parent themselves. A row
/// rendered under a different parent than the task's own (its parent is
/// hidden by the type filter, so it shows as a root) is left alone: nothing
/// is written and no error is shown, because Core would move the task
/// through levels the list does not display. On indent the new parent (the
/// previous live sibling, read from the pre-move sibling order) is expanded
/// so the moved task stays visible. Selection follows the task to its new
/// row. `Ok(false)` is a silent no-op; Core errors are surfaced via
/// `app.error`.
fn reparent_focused_task<S: Store>(app: &mut App, core: &mut Core<S>, kind: Reparent) {
    let Some((id, rendered_parent)) = app.selected_row().map(|row| (row.id, row.parent_id)) else {
        return;
    };
    let parent = app
        .tasks
        .iter()
        .find(|task| task.id == id)
        .and_then(|task| task.parent_id);
    if rendered_parent != parent {
        return;
    }
    let (result, expand) = match kind {
        Reparent::Indent => {
            // Re-read the order rather than trusting `app.sibling_order`,
            // which paths like delete leave stale, so the row expanded is
            // the parent Core will actually pick.
            if let Ok(order) = core.sibling_order() {
                app.sibling_order = order;
            }
            // The row is rendered under the task's own parent here, so the
            // siblings Core indents among are that parent's children.
            let siblings = app.sibling_order.children_of(parent);
            let target = siblings
                .iter()
                .position(|&s| s == id)
                .and_then(|i| i.checked_sub(1))
                .map(|i| siblings[i]);
            (core.indent_task(id), target)
        }
        Reparent::Outdent => (core.outdent_task(id), None),
    };
    match result {
        Ok(false) => {}
        Ok(true) => {
            app.error = None;
            if let Some(target) = expand {
                app.collapsed.remove(&target);
            }
            refresh_rows_from_tree(app, core, Some(id));
        }
        Err(err) => app.error = Some(err.to_string()),
    }
}

/// Handles `Action::CollapseFocused`: no-op with no selection, no-op when
/// the focused row has no children, no-op (idempotent) when it's already
/// collapsed. Otherwise inserts its id into `app.collapsed` and rebuilds
/// `app.rows`.
fn collapse_focused(app: &mut App) {
    let Some(row) = app.selected_row() else {
        return;
    };
    if !row.has_children {
        return;
    }
    let id = row.id;
    if !app.collapsed.insert(id) {
        return;
    }
    app.rebuild_rows();
}

/// Handles `Action::ExpandFocused`: no-op with no selection, no-op when the
/// focused row isn't currently collapsed. Otherwise removes its id from
/// `app.collapsed` and rebuilds `app.rows`.
fn expand_focused(app: &mut App) {
    let Some(row) = app.selected_row() else {
        return;
    };
    let id = row.id;
    if !app.collapsed.remove(&id) {
        return;
    }
    app.rebuild_rows();
}

/// Handles `Action::ExpandAll`: clears `app.collapsed` entirely and rebuilds
/// `app.rows`. Called unconditionally, even when `app.collapsed` is already
/// empty — clearing an empty set is itself a correct no-op, and
/// `rebuild_rows` on unchanged state just reselects the same row.
fn expand_all(app: &mut App) {
    app.collapsed.clear();
    app.rebuild_rows();
}

/// Handles `Action::CollapseAll`: sets `app.collapsed` to every `TaskId` in
/// `app.tasks` that has at least one child (i.e. is named in some other
/// task's `parent_id`), then rebuilds `app.rows`.
fn collapse_all(app: &mut App) {
    let parents_with_children: HashSet<TaskId> =
        app.tasks.iter().filter_map(|task| task.parent_id).collect();
    app.collapsed = parents_with_children;
    app.rebuild_rows();
}

/// Handles `Action::CycleTypeFilter`: advances `app.type_filter` through
/// `None -> available_type_keys[0] -> available_type_keys[1] -> ... ->
/// None`. When `app.type_filter` is `Some(current)` but `current` is no
/// longer present in `available_type_keys` (e.g. the type was since removed
/// from the configured list), wraps to `None` defensively rather than
/// getting stuck — the same treatment as "`current` was the last key".
/// Doesn't itself refresh `app.rows`; callers (`apply_action`) follow this
/// with `refresh_rows_from_tree`, which needs `&mut Core<S>` that this plain
/// `&mut App` signature doesn't have.
fn cycle_type_filter(app: &mut App) {
    app.type_filter = match &app.type_filter {
        None => app.available_type_keys.first().cloned(),
        Some(current) => {
            let next_index = app
                .available_type_keys
                .iter()
                .position(|key| key == current)
                .map(|index| index + 1);
            next_index.and_then(|index| app.available_type_keys.get(index).cloned())
        }
    };
}

/// Handles `Action::ToggleComplete`: no-op with no selection. For an
/// `Incomplete` task, calls `Core::complete_task(id, false)` — on success
/// refreshes every touched row's status (a single-element result for a leaf
/// task); on `CoreError::IncompleteChildren`, enters `Mode::Confirm` with
/// `PendingAction::CompleteCascade(id)` and a prompt naming the task and the
/// incomplete-child count; on any other error sets `app.error`. For a
/// `Complete` task, calls `Core::reopen_task(id)` and refreshes that task's
/// rows on success (mirroring the same error-handling shape). Either way a
/// successful call also re-reads the tree from `Core`, since changing a
/// task's status changes which *other* tasks are blocked.
fn handle_toggle_complete<S: Store>(app: &mut App, core: &mut Core<S>) {
    let Some(row) = app.selected_row() else {
        return;
    };
    let id = row.id;
    let title = row.title.clone();

    match row.status {
        TaskStatus::Incomplete => match core.complete_task(id, false) {
            Ok(touched) => {
                refresh_row_statuses(app, &touched);
                app.error = None;
                refresh_rows_from_tree(app, core, Some(id));
            }
            Err(CoreError::IncompleteChildren { incomplete, .. }) => {
                let count = incomplete.len();
                app.mode = Mode::Confirm {
                    prompt: format!(
                        "Complete \"{title}\"? {count} incomplete subtask(s) will also be completed. (y/n)"
                    ),
                    action: PendingAction::CompleteCascade(id),
                };
            }
            Err(err) => {
                app.error = Some(err.to_string());
            }
        },
        TaskStatus::Complete => match core.reopen_task(id) {
            Ok(task) => {
                refresh_row_statuses(app, std::slice::from_ref(&task));
                app.error = None;
                refresh_rows_from_tree(app, core, Some(id));
            }
            Err(err) => {
                app.error = Some(err.to_string());
            }
        },
    }
}

/// Updates the `status` of each task in `touched` on its row in `app.rows`,
/// matched by id (a task has one row, or none while hidden). Shared by
/// [`handle_toggle_complete`] and [`confirm_yes`]'s `CompleteCascade` arm so
/// both "refresh the rows a `Core` call actually touched" call sites use the
/// same lookup-by-id loop.
///
/// Also updates the matching entries in `app.tasks`, not just `app.rows`:
/// `app.tasks` is the cache `rebuild_rows` (collapse/expand/expand-all/
/// collapse-all) re-derives `app.rows` from, so leaving it stale here would
/// mean a later collapse/expand silently reverts the status change just
/// applied to `app.rows`, and would also corrupt `direct_summary`'s
/// complete/total counts (computed from `app.tasks`' child statuses) on a
/// collapsed parent.
fn refresh_row_statuses(app: &mut App, touched: &[bala_core::Task]) {
    for task in touched {
        if let Some(row) = app.rows.iter_mut().find(|row| row.id == task.id) {
            row.status = task.status;
        }
        if let Some(cached) = app.tasks.iter_mut().find(|cached| cached.id == task.id) {
            cached.status = task.status;
        }
    }
}

/// Handles `Action::StartInsertNewTitle`: enters `Mode::Insert` with an
/// empty buffer, scoped to `EditableField::NewTitle`.
fn start_insert_new_title(app: &mut App) {
    app.mode = Mode::Insert {
        field: EditableField::NewTitle,
        buffer: String::new(),
    };
    app.error = None;
}

/// Handles `Action::InsertChar`: appends `c` to the current `Mode::Insert`
/// buffer. No-op outside `Mode::Insert`.
fn insert_char(app: &mut App, c: char) {
    if let Mode::Insert { buffer, .. } = &mut app.mode {
        buffer.push(c);
    }
}

/// Handles `Action::Backspace`: removes the last character from the current
/// `Mode::Insert` buffer. No-op outside `Mode::Insert`.
fn backspace(app: &mut App) {
    if let Mode::Insert { buffer, .. } = &mut app.mode {
        buffer.pop();
    }
}

/// Handles `Action::EnterDetail`: switches to the Detail pane, defaulting
/// the cursor to `Title`. No-op when there's no selection.
fn enter_detail(app: &mut App) {
    if app.selected_row().is_some() {
        app.pane = Pane::Detail;
        app.detail_field = DetailField::Title;
        app.error = None;
    }
}

/// Handles `Action::StartInsertNewSubtask`: enters `Mode::Insert` scoped to
/// the selected task as the new subtask's parent. No-op when there's no
/// selection (matching `EnterDetail`'s precedent).
fn start_insert_new_subtask(app: &mut App) {
    if let Some(row) = app.selected_row() {
        let parent_id = row.id;
        app.mode = Mode::Insert {
            field: EditableField::NewSubtaskTitle(parent_id),
            buffer: String::new(),
        };
        app.error = None;
    }
}

/// Handles `Action::StartReparent`: enters `Mode::Insert` scoped to the
/// selected task, prefilled with its current parent id (empty string when
/// it's already top-level). No-op when there's no selection, or when a fresh
/// `Core::get_task` fetch for the selected id comes back `Ok(None)` (the
/// task vanished out from under the list — nothing sensible to prefill, so
/// we leave `app` in `Mode::Normal` rather than entering Insert with stale
/// data). A genuine backend failure (`Err`) is surfaced via `app.error`
/// rather than silently treated the same as a missing task.
fn start_reparent<S: Store>(app: &mut App, core: &Core<S>) {
    let Some(row) = app.selected_row() else {
        return;
    };
    let id = row.id;
    let task = match core.get_task(id) {
        Ok(Some(task)) => task,
        Ok(None) => return,
        Err(err) => {
            app.error = Some(err.to_string());
            return;
        }
    };

    let buffer = task
        .parent_id
        .map(|parent_id| uuid::Uuid::from(parent_id).to_string())
        .unwrap_or_default();

    app.mode = Mode::Insert {
        field: EditableField::Parent(id),
        buffer,
    };
    app.error = None;
}

/// Handles `Action::StartSetType`: enters `Mode::Insert` scoped to the
/// selected task, prefilled with its current `type_key` (fetched fresh via
/// `Core::get_task`). No-op when there's no selection, or when a fresh
/// `Core::get_task` fetch for the selected id comes back `Ok(None)` (the
/// task vanished out from under the list — nothing sensible to prefill, so
/// we leave `app` in `Mode::Normal` rather than entering Insert with stale
/// data). A genuine backend failure (`Err`) is surfaced via `app.error`
/// rather than silently treated the same as a missing task.
fn start_set_type<S: Store>(app: &mut App, core: &Core<S>) {
    let Some(row) = app.selected_row() else {
        return;
    };
    let id = row.id;
    let task = match core.get_task(id) {
        Ok(Some(task)) => task,
        Ok(None) => return,
        Err(err) => {
            app.error = Some(err.to_string());
            return;
        }
    };

    app.mode = Mode::Insert {
        field: EditableField::TypeKey(id),
        buffer: task.type_key.clone(),
    };
    app.error = None;
}

/// Handles `Action::StartAddDependency`: enters `Mode::Insert` scoped to the
/// selected task with an empty buffer, ready for the id of the task it will
/// depend on. No-op when there's no selection.
fn start_add_dependency(app: &mut App) {
    let Some(row) = app.selected_row() else {
        return;
    };
    app.mode = Mode::Insert {
        field: EditableField::AddPredecessor(row.id),
        buffer: String::new(),
    };
    app.error = None;
}

/// Handles `Action::StartRemoveDependency`: enters `Mode::Insert` scoped to
/// the selected task, ready for the id of the predecessor to drop. The
/// buffer is prefilled with that id when the task (fetched fresh via
/// `Core::get_task`) depends on exactly one task, and empty otherwise, since
/// with several there is no single obvious choice. No-op when there's no
/// selection, or when the fetch comes back `Ok(None)` (the task vanished out
/// from under the list), as in `start_reparent`. A backend failure (`Err`)
/// is surfaced via `app.error`.
fn start_remove_dependency<S: Store>(app: &mut App, core: &Core<S>) {
    let Some(row) = app.selected_row() else {
        return;
    };
    let id = row.id;
    let task = match core.get_task(id) {
        Ok(Some(task)) => task,
        Ok(None) => return,
        Err(err) => {
            app.error = Some(dependency_error(app, core, &err));
            return;
        }
    };

    let buffer = match task.depends_on.as_slice() {
        [only] => uuid::Uuid::from(only.predecessor_id).to_string(),
        _ => String::new(),
    };

    app.mode = Mode::Insert {
        field: EditableField::RemovePredecessor(id),
        buffer,
    };
    app.error = None;
}

/// Handles `Action::StartEditTitle`: enters `Mode::Insert` prefilled with
/// the selected task's current title. No-op when there's no selection.
fn start_edit_title(app: &mut App) {
    if let Some(row) = app.selected_row() {
        let id = row.id;
        let title = row.title.clone();
        app.mode = Mode::Insert {
            field: EditableField::Title(id),
            buffer: title,
        };
        app.error = None;
    }
}

/// Handles `Action::StartEditDescription`: enters `Mode::Insert` prefilled
/// with the selected task's current description (empty string when it has
/// none). No-op when there's no selection.
fn start_edit_description(app: &mut App) {
    if let Some(row) = app.selected_row() {
        let id = row.id;
        let buffer = app.description_of(id).unwrap_or_default().to_string();
        app.mode = Mode::Insert {
            field: EditableField::Description(id),
            buffer,
        };
        app.error = None;
    }
}

/// Handles `Action::DKeyPressed` given whether a `d` was already pending
/// (`was_pending_d`, read from `App.pending_d` before `apply_action` reset
/// it for this action). On the first press, sets `app.pending_d` so the
/// next `DKeyPressed` is recognized as the second. On the second press
/// with no selection it's a no-op; otherwise it enters `Mode::Confirm`
/// naming the selected task.
///
/// Which prompt depends on the task's live direct children, fetched via
/// `Core::list_children` rather than read from `app.rows`, so a parent whose
/// children are all hidden by the type filter (or collapsed) still gets the
/// choice. With no children the question is a plain `y`/`n` and the pending
/// action is `PendingAction::Delete`; with children it names the child
/// count and offers `s` (delete the subtree), `p` (promote the children) or
/// `n` (cancel), and the pending action is
/// `PendingAction::DeleteWithChildren`.
///
/// The question is the prompt's last line. Above it [`delete_prompt`] names
/// the tasks that depend on what the delete covers, found in the unfiltered
/// tree (see `unfiltered_tree`) so a dependent the type filter hides is
/// still named. If listing the children or fetching that tree fails,
/// `app.error` is set and `app` stays in `Mode::Normal`.
fn handle_d_key_pressed<S: Store>(app: &mut App, core: &Core<S>, was_pending_d: bool) {
    if !was_pending_d {
        app.pending_d = true;
        return;
    }
    let Some(row) = app.selected_row() else {
        return;
    };
    let id = row.id;
    let prompt = core.list_children(id).and_then(|children| {
        let tasks = unfiltered_tree(app, core)?;
        Ok((
            delete_prompt(id, &row.title, children.len(), &tasks),
            children.is_empty(),
        ))
    });
    match prompt {
        Ok((prompt, is_leaf)) => {
            let action = if is_leaf {
                PendingAction::Delete(id)
            } else {
                PendingAction::DeleteWithChildren(id)
            };
            app.mode = Mode::Confirm { prompt, action };
        }
        Err(err) => app.error = Some(err.to_string()),
    }
}

/// How many titles each group of [`delete_prompt`] lists before it
/// summarizes the rest as a count.
const MAX_PROMPT_DEPENDENTS: usize = 5;

/// Builds the `dd` confirmation prompt for the task `id` titled `title`,
/// which has `child_count` live direct children, from `tasks`, the
/// unfiltered tree.
///
/// The last line is the question: a plain `y`/`n` for a childless task,
/// otherwise the child count and the subtree/promote/cancel choice. Above
/// it come up to two groups of dependents, each a header line with the
/// group's size followed by one indented title per line, capped at
/// [`MAX_PROMPT_DEPENDENTS`] titles and then an `… and N more` line:
///
/// 1. the tasks that depend on `id` itself (`render::dependents_of`), which
///    either kind of delete leaves depending on a deleted task;
/// 2. for a task with children, the other tasks outside its subtree that
///    depend on one of its descendants (`render::subtree_ids`), which only a
///    subtree delete affects.
///
/// An empty group is omitted, header included, so a task nothing depends on
/// gets the question alone.
fn delete_prompt(id: TaskId, title: &str, child_count: usize, tasks: &[Task]) -> String {
    let mut lines = Vec::new();
    let on_task = render::dependents_of(&[id], tasks);
    push_dependent_lines(&mut lines, "task(s) depend on this task:", &on_task);
    if child_count == 0 {
        lines.push(format!("Delete \"{title}\"? (y/n)"));
    } else {
        let on_subtasks: Vec<&Task> = render::dependents_of(&render::subtree_ids(id, tasks), tasks)
            .into_iter()
            .filter(|task| !on_task.iter().any(|named| named.id == task.id))
            .collect();
        push_dependent_lines(
            &mut lines,
            "more depend on its subtasks (subtree delete only):",
            &on_subtasks,
        );
        lines.push(format!(
            "\"{title}\" has {child_count} subtask(s). Delete [s]ubtree, [p]romote children, or [n] cancel?"
        ));
    }
    lines.join("\n")
}

/// Appends one group of [`delete_prompt`] to `lines`: a `"{count} {label}"`
/// header, then the first [`MAX_PROMPT_DEPENDENTS`] titles of `dependents`
/// indented two spaces, then a count of any left over. Appends nothing for
/// an empty group.
fn push_dependent_lines(lines: &mut Vec<String>, label: &str, dependents: &[&Task]) {
    if dependents.is_empty() {
        return;
    }
    lines.push(format!("{} {label}", dependents.len()));
    lines.extend(
        dependents
            .iter()
            .take(MAX_PROMPT_DEPENDENTS)
            .map(|task| format!("  {}", task.title)),
    );
    let rest = dependents.len().saturating_sub(MAX_PROMPT_DEPENDENTS);
    if rest > 0 {
        lines.push(format!("  … and {rest} more"));
    }
}

/// Handles `Action::ConfirmYes`: runs the `Mode::Confirm`'s `PendingAction`.
///
/// For `PendingAction::Delete(id)` — `dd` on a task that had no children
/// when the prompt opened — deletes it via [`confirm_delete`] with
/// `DeleteMode::PromoteChildren`. For a childless task that deletes just the
/// task, same as `DeleteMode::Subtree` would; but the child check and the
/// delete run in separate transactions, so if another writer sharing the
/// store has since given the task children, promoting them keeps them
/// rather than deleting a subtree the prompt never mentioned. A task with
/// children is pending as `PendingAction::DeleteWithChildren` instead,
/// which `y` does not answer: its prompt takes `s`/`p`
/// (`Action::ConfirmDelete`) so the user chooses explicitly between
/// deleting the subtree and promoting the children, and `ConfirmYes` leaves
/// it pending.
///
/// For `PendingAction::CompleteCascade(id)`, calls
/// `Core::complete_task(id, true)`. On success every touched row's status is
/// refreshed (same lookup-by-id loop as [`handle_toggle_complete`]'s
/// non-cascade path) and `app` returns to `Mode::Normal`, leaving `app.pane`
/// untouched — unlike a delete, completing a task doesn't invalidate its
/// Detail view. On failure `app.error` is set and `app` returns to
/// `Mode::Normal`.
fn confirm_yes<S: Store>(app: &mut App, core: &mut Core<S>) {
    let Mode::Confirm { action, .. } = &app.mode else {
        return;
    };

    match *action {
        PendingAction::Delete(id) => confirm_delete(app, core, id, DeleteMode::PromoteChildren),
        PendingAction::CompleteCascade(id) => match core.complete_task(id, true) {
            Ok(touched) => {
                refresh_row_statuses(app, &touched);
                app.error = None;
                refresh_rows_from_tree(app, core, Some(id));
                app.mode = Mode::Normal;
            }
            Err(err) => {
                app.mode = Mode::Normal;
                app.error = Some(err.to_string());
            }
        },
        PendingAction::InheritFromParent { child, parent } => {
            inherit_from_parent(app, core, child, parent);
            app.mode = Mode::Normal;
        }
        PendingAction::DeleteWithChildren(_) => {}
    }
}

/// Deletes `id` via `Core::delete_task(id, mode)` and leaves the delete
/// prompt. Shared by `ConfirmYes` on a `PendingAction::Delete` (always
/// `DeleteMode::PromoteChildren`) and `ConfirmDelete(mode)` on a
/// `PendingAction::Delete` or `PendingAction::DeleteWithChildren`.
///
/// With `DeleteMode::Subtree`, every descendant of `id` is deleted too.
/// With `DeleteMode::PromoteChildren` only `id` is deleted and its children
/// are reparented to `id`'s own parent, or made top-level if it had none.
///
/// On success, `app.rows` is rebuilt from a fresh tree via
/// `refresh_rows_from_tree` rather than patched: promoted children move to
/// a new position and depth, and a surviving parent's `has_children` flag
/// and rolled-up progress can change, so only a full re-render gets the
/// affected rows right. `app.error` is cleared before that refresh, so an error the
/// refresh itself sets (e.g. the follow-up `get_tree` failing after the
/// delete committed) is preserved; in that case the cached rows are stale,
/// so the tasks in `DeleteOutcome::deleted` are dropped from them rather
/// than left on screen to act on. The selection keeps its previous index,
/// clamped to the new rows, and `app` returns to `Mode::Normal`/`Pane::List`
/// (the deleted task's Detail view no longer makes sense). On failure
/// `app.error` is set and `app` still returns to `Mode::Normal` — there's
/// no in-progress input to preserve here, unlike `submit_insert`'s failure
/// path.
fn confirm_delete<S: Store>(app: &mut App, core: &mut Core<S>, id: TaskId, mode: DeleteMode) {
    match core.delete_task(id, mode) {
        Ok(outcome) => {
            app.error = None;
            let previous = app.selected;
            refresh_rows_from_tree(app, core, None);
            if app.error.is_some() {
                // The delete committed but the re-fetch failed, so the
                // cached rows are stale: at least drop the tombstoned
                // tasks, which would otherwise stay on screen and fail
                // any action taken on them with `NotFound`.
                let deleted: HashSet<TaskId> = outcome.deleted.iter().map(|task| task.id).collect();
                app.rows.retain(|row| !deleted.contains(&row.id));
                app.tasks.retain(|task| !deleted.contains(&task.id));
            }
            app.selected = app
                .rows
                .len()
                .checked_sub(1)
                .map(|last| previous.unwrap_or(0).min(last));
            app.pane = Pane::List;
            app.mode = Mode::Normal;
        }
        Err(err) => {
            app.mode = Mode::Normal;
            app.error = Some(err.to_string());
        }
    }
}

/// Handles `PendingAction::InheritFromParent`'s `ConfirmYes` path: refetches
/// `parent` and patches `child` with its assignee/dates via
/// `Core::update_task`. If `parent` has since vanished, there's nothing
/// sensible to inherit, so `child` is left as created; a genuine backend
/// failure on the refetch is surfaced via `app.error` rather than treated
/// the same as a missing parent. On an `update_task` failure `app.error` is
/// set; on success `app.rows` is refreshed via `refresh_rows_from_tree` so
/// the child's row reflects any newly-set assignee immediately — any error
/// `refresh_rows_from_tree` itself sets (e.g. the follow-up `get_tree`
/// failing) is preserved rather than immediately overwritten, since the
/// mutation already committed and the user still needs to know the
/// displayed rows may now be stale.
fn inherit_from_parent<S: Store>(app: &mut App, core: &mut Core<S>, child: TaskId, parent: TaskId) {
    let parent_task = match core.get_task(parent) {
        Ok(Some(parent_task)) => parent_task,
        Ok(None) => return,
        Err(err) => {
            app.error = Some(err.to_string());
            return;
        }
    };

    let patch = TaskPatch {
        assignee_id: parent_task.assignee_id.map_or(Field::Keep, Field::Set),
        start_date: parent_task.start_date.map_or(Field::Keep, Field::Set),
        due_date: parent_task.due_date.map_or(Field::Keep, Field::Set),
        ..Default::default()
    };

    match core.update_task(child, patch) {
        Ok(_) => {
            app.error = None;
            refresh_rows_from_tree(app, core, Some(child));
        }
        Err(err) => {
            app.error = Some(err.to_string());
        }
    }
}

/// Handles `Action::SubmitInsert` for every `EditableField` variant: creates
/// a new top-level task for `NewTitle`, or patches an existing task's title/
/// description for `Title(id)`/`Description(id)`. On success, `app` returns
/// to `Mode::Normal` — leaving `app.pane` untouched, so a `Title`/
/// `Description` edit (only ever started from the Detail pane) lands back
/// in the Detail pane showing the refreshed value, per the same "return to
/// where you were" logic as `CancelInsert`. On failure `app.error` is set
/// and `app` stays in `Mode::Insert` with the buffer intact so the user can
/// fix and resubmit.
fn submit_insert<S: Store>(app: &mut App, core: &mut Core<S>) {
    let Mode::Insert { field, buffer } = &app.mode else {
        return;
    };

    match *field {
        EditableField::NewTitle => submit_new_title(app, core, buffer.clone()),
        EditableField::Title(id) => submit_edit_title(app, core, id, buffer.clone()),
        EditableField::Description(id) => submit_edit_description(app, core, id, buffer.clone()),
        EditableField::NewSubtaskTitle(parent_id) => {
            submit_new_subtask(app, core, parent_id, buffer.clone());
        }
        EditableField::Parent(id) => submit_reparent(app, core, id, &buffer.clone()),
        EditableField::TypeKey(id) => submit_set_type(app, core, id, &buffer.clone()),
        EditableField::AddPredecessor(id) => submit_add_dependency(app, core, id, &buffer.clone()),
        EditableField::RemovePredecessor(id) => {
            submit_remove_dependency(app, core, id, &buffer.clone());
        }
    }
}

/// `EditableField::Parent(id)`: parses `buffer` as a single parent UUID (an
/// empty/whitespace-only buffer means "no parent", i.e. promote `id` to
/// top-level) and calls `Core::set_parent` with it. On success,
/// refreshes `app.rows` from the full tree via `refresh_rows_from_tree` so
/// `id` reappears at its new nested (or top-level) position, and returns to
/// `Mode::Normal`. On more than one id (entries separated by commas or
/// whitespace — a task has at most one parent), a parse failure (an entry
/// that isn't a valid UUID) or a `CoreError` from `set_parent` (e.g.
/// `CircularHierarchy`), sets
/// `app.error` and leaves `app.mode` untouched so the buffer survives for
/// correction — matching `submit_new_title`'s/`submit_new_subtask`'s
/// existing "failure leaves mode untouched" convention. Any error
/// `refresh_rows_from_tree` itself sets on the success path is preserved,
/// not immediately cleared — the reparent already committed, so the user
/// still needs to see that the displayed rows may now be stale.
fn submit_reparent<S: Store>(app: &mut App, core: &mut Core<S>, id: TaskId, buffer: &str) {
    let mut entries = buffer
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|entry| !entry.is_empty());
    let new_parent = match (entries.next(), entries.next()) {
        (None, _) => None,
        (Some(_), Some(_)) => {
            app.error = Some("a task can have only one parent".to_string());
            return;
        }
        (Some(entry), None) => {
            let Ok(parent_id) = entry.parse::<uuid::Uuid>() else {
                app.error = Some("invalid parent task id".to_string());
                return;
            };
            Some(TaskId::from(parent_id))
        }
    };

    match core.set_parent(id, new_parent) {
        Ok(_) => {
            app.error = None;
            refresh_rows_from_tree(app, core, Some(id));
            app.mode = Mode::Normal;
        }
        Err(err) => {
            app.error = Some(err.to_string());
        }
    }
}

/// `EditableField::AddPredecessor(id)`: parses `buffer` as exactly one
/// predecessor UUID and calls `Core::add_dependency` with it and
/// `DependencyType::FinishToStart`, making `id` depend on that task. On
/// success, refreshes `app.rows` from the full tree via
/// `refresh_rows_from_tree` so `id`'s row picks up its blocked flag, and
/// returns to `Mode::Normal`.
///
/// `id` is re-read first and the typed id checked against its `depends_on`:
/// `Core::add_dependency` upserts, so adding an edge that already exists
/// would silently rewrite its type to finish-to-start, and a repeated id
/// would look like a new dependency. The check is against `depends_on`
/// itself, so an edge to a since-deleted task counts too.
///
/// On an empty buffer, more than one id (entries separated by commas or
/// whitespace — each submit adds one dependency), a parse failure (an entry
/// that isn't a valid UUID), an id `id` already depends on, `id` itself
/// having vanished, or a `CoreError` (e.g. `CircularDependency`, worded by
/// `dependency_error`), sets `app.error` and leaves `app.mode`
/// untouched so the buffer survives for correction, as `submit_reparent`
/// does. Any error `refresh_rows_from_tree` itself sets on the success path
/// is preserved, not immediately cleared — the dependency already committed,
/// so the user still needs to see that the displayed rows may now be stale.
fn submit_add_dependency<S: Store>(app: &mut App, core: &mut Core<S>, id: TaskId, buffer: &str) {
    let predecessor = match parse_one_task_id(buffer) {
        Ok(predecessor) => predecessor,
        Err(message) => {
            app.error = Some(message.to_string());
            return;
        }
    };

    let task = match core.get_task(id) {
        Ok(Some(task)) => task,
        Ok(None) => {
            app.error = Some(TASK_GONE_MESSAGE.to_string());
            return;
        }
        Err(err) => {
            app.error = Some(dependency_error(app, core, &err));
            return;
        }
    };
    if task
        .depends_on
        .iter()
        .any(|dep| dep.predecessor_id == predecessor)
    {
        app.error = Some("this task already depends on that task".to_string());
        return;
    }

    match core.add_dependency(id, predecessor, DependencyType::FinishToStart) {
        Ok(_) => {
            app.error = None;
            refresh_rows_from_tree(app, core, Some(id));
            app.mode = Mode::Normal;
        }
        // `NotFound` carries whichever task is missing, so one naming `id`
        // means the task was deleted between the read above and this call,
        // not that the typed id is wrong.
        Err(CoreError::NotFound(missing)) if missing == id => {
            app.error = Some(TASK_GONE_MESSAGE.to_string());
        }
        Err(err) => {
            app.error = Some(dependency_error(app, core, &err));
        }
    }
}

/// Shown under a dependency entry line when the task it was opened for has
/// been deleted since.
const TASK_GONE_MESSAGE: &str = "this task no longer exists";

/// `EditableField::RemovePredecessor(id)`: parses `buffer` as exactly one
/// predecessor UUID and calls `Core::remove_dependency` with it, so `id` no
/// longer depends on that task. On success, refreshes `app.rows` from the
/// full tree via `refresh_rows_from_tree` so `id`'s row drops its blocked
/// flag once nothing incomplete is left in front of it, and returns to
/// `Mode::Normal`.
///
/// `id` is re-read first and the typed id checked against its `depends_on`:
/// `Core::remove_dependency` treats an edge that doesn't exist as a silent
/// no-op, so without the check a mistyped id would look like a successful
/// removal. The check is against `depends_on` itself, not the live tasks, so
/// a dependency on a since-deleted task can still be removed.
///
/// On an empty buffer, more than one id, a parse failure, an id `id` does
/// not depend on, `id` itself having vanished, or a `CoreError` (worded by
/// `dependency_error`), sets `app.error` and leaves `app.mode`
/// untouched so the buffer survives for correction, as
/// `submit_add_dependency` does. Any error `refresh_rows_from_tree` itself
/// sets on the success path is preserved, as there.
fn submit_remove_dependency<S: Store>(app: &mut App, core: &mut Core<S>, id: TaskId, buffer: &str) {
    let predecessor = match parse_one_task_id(buffer) {
        Ok(predecessor) => predecessor,
        Err(message) => {
            app.error = Some(message.to_string());
            return;
        }
    };

    let task = match core.get_task(id) {
        Ok(Some(task)) => task,
        Ok(None) => {
            app.error = Some(TASK_GONE_MESSAGE.to_string());
            return;
        }
        Err(err) => {
            app.error = Some(dependency_error(app, core, &err));
            return;
        }
    };
    if !task
        .depends_on
        .iter()
        .any(|dep| dep.predecessor_id == predecessor)
    {
        app.error = Some("this task does not depend on that task".to_string());
        return;
    }

    match core.remove_dependency(id, predecessor) {
        Ok(_) => {
            app.error = None;
            refresh_rows_from_tree(app, core, Some(id));
            app.mode = Mode::Normal;
        }
        // The task was deleted between the read above and this call.
        Err(CoreError::NotFound(missing)) if missing == id => {
            app.error = Some(TASK_GONE_MESSAGE.to_string());
        }
        Err(err) => {
            app.error = Some(dependency_error(app, core, &err));
        }
    }
}

/// Parses a dependency entry line's `buffer` as exactly one full task id.
/// Entries are separated by commas or whitespace, as in `submit_reparent`.
/// An empty buffer, more than one entry, or an entry that isn't a valid
/// UUID is an `Err` holding the one-line message to show under the line.
fn parse_one_task_id(buffer: &str) -> Result<TaskId, &'static str> {
    let mut entries = buffer
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|entry| !entry.is_empty());
    match (entries.next(), entries.next()) {
        (None, _) => Err("enter a task id"),
        (Some(_), Some(_)) => Err("enter one task id at a time"),
        (Some(entry), None) => entry
            .parse::<uuid::Uuid>()
            .map(TaskId::from)
            .map_err(|_| "invalid task id"),
    }
}

/// Words `err` with [`dependency_error_message`], resolving task titles from
/// the unfiltered tree (see `unfiltered_tree`) so a task the type filter
/// hides from `app.tasks` is still named by title rather than by a UUID too
/// long for the error line. The tree is only consulted for the variants
/// that name other tasks, and only fetched when the cache is filtered; if
/// that fetch fails, the titles come from `app.tasks` alone.
fn dependency_error<S: Store>(app: &App, core: &Core<S>, err: &CoreError) -> String {
    let names_tasks = matches!(
        err,
        CoreError::DependsOnRelative { .. } | CoreError::CircularDependency { .. }
    );
    if names_tasks && let Ok(tasks) = unfiltered_tree(app, core) {
        return dependency_error_message(err, &tasks);
    }
    dependency_error_message(err, &app.tasks)
}

/// The one-line text shown under a dependency entry line for an `err` from
/// `Core`, per LLD §Error Rendering. `CoreError`'s own
/// text names tasks as `TaskId(…)` debug values, which neither reads well
/// nor fits the error area, so the variants `add_dependency` raises are
/// reworded here to name each task by its title in `tasks`. A task missing
/// from `tasks` (soft-deleted, or hidden by the type filter when `tasks` is
/// a filtered tree) is named by its plain UUID instead. A
/// `CircularDependency`'s `cycle` already starts and
/// ends with the task gaining the dependency, so it is printed as given.
/// Any other variant keeps `err.to_string()`.
fn dependency_error_message(err: &CoreError, tasks: &[Task]) -> String {
    let name = |id: TaskId| {
        tasks.iter().find(|task| task.id == id).map_or_else(
            || uuid::Uuid::from(id).to_string(),
            |task| task.title.clone(),
        )
    };
    match err {
        CoreError::SelfDependency(_) => "a task cannot depend on itself".to_string(),
        CoreError::DependsOnRelative { other, .. } => {
            format!(
                "\"{}\" is an ancestor or descendant of this task",
                name(*other)
            )
        }
        CoreError::CircularDependency { cycle } => {
            let path: Vec<String> = cycle.iter().map(|id| name(*id)).collect();
            format!("would create a cycle: {}", path.join(" → "))
        }
        CoreError::NotFound(_) => "no task with that id".to_string(),
        _ => err.to_string(),
    }
}

/// `EditableField::TypeKey(id)`: trims `buffer` and calls `Core::update_task`
/// with `TaskPatch { type_key: Field::Set(trimmed), .. }`. On success,
/// refreshes `app.rows` from the full tree via `refresh_rows_from_tree` so
/// `id`'s row reflects the new type label, and returns to `Mode::Normal`. On
/// a `CoreError` from `update_task` (e.g. `UnknownTaskType` when the typed
/// key names no configured type), sets `app.error` and leaves `app.mode`
/// untouched so the buffer survives for correction — matching
/// `submit_reparent`'s existing "failure leaves mode untouched" convention.
/// Any error `refresh_rows_from_tree` itself sets on the success path is
/// preserved, not immediately cleared — the type change already committed,
/// so the user still needs to see that the displayed rows may now be stale.
fn submit_set_type<S: Store>(app: &mut App, core: &mut Core<S>, id: TaskId, buffer: &str) {
    let trimmed = buffer.trim().to_string();

    match core.update_task(
        id,
        TaskPatch {
            type_key: Field::Set(trimmed),
            ..Default::default()
        },
    ) {
        Ok(_) => {
            app.error = None;
            refresh_rows_from_tree(app, core, Some(id));
            app.mode = Mode::Normal;
        }
        Err(err) => {
            app.error = Some(err.to_string());
        }
    }
}

/// Refetches the task tree via `Core::get_tree`, filtered by
/// `app.type_filter` (`None` filters nothing), caches it as `app.tasks`
/// (recording that filter in `app.tasks_filter`; a failed fetch leaves both
/// as they were),
/// rebuilds `app.rows` via `render::task_rows` (so a newly created/
/// reparented task lands at its correct nested position, honoring
/// `app.collapsed`), and merges every fetched task's description into
/// `app.descriptions` — a task that a filter previously excluded may have
/// never been cached, and re-fetching it via a filter change (e.g. `f`)
/// must not leave a stale `None` behind for `description_of` to hand
/// `start_edit_description` as if the task genuinely had no description.
///
/// Selection: `select_id`, when given, is selected if it's present in the
/// new `app.rows`. Otherwise — including when `select_id` is `None`, or
/// names a task the new filter excludes — the selection resets to the
/// first remaining row, or `None` if the new result set is empty; unlike
/// `App::select_by_id`'s own "leave selection untouched" default (correct
/// for restoring a persisted startup selection), silently keeping a stale
/// numeric index here would leave it dangling past the end of a shrunk row
/// list, or pointing at an unrelated row of the same length.
fn refresh_rows_from_tree<S: Store>(app: &mut App, core: &mut Core<S>, select_id: Option<TaskId>) {
    let fetched = core
        .get_tree(bala_core::TreeFilter {
            type_key: app.type_filter.clone(),
            ..Default::default()
        })
        .and_then(|tasks| core.sibling_order().map(|order| (tasks, order)));
    match fetched {
        Ok((tasks, sibling_order)) => {
            for task in &tasks {
                app.descriptions.insert(task.id, task.description.clone());
            }
            app.tasks = tasks;
            app.tasks_filter.clone_from(&app.type_filter);
            app.sibling_order = sibling_order;
            app.rows = app.build_rows();
            app.selected = select_id
                .and_then(|id| app.rows.iter().position(|row| row.id == id))
                .or(if app.rows.is_empty() { None } else { Some(0) });
        }
        Err(err) => {
            app.error = Some(err.to_string());
        }
    }
}

/// `EditableField::NewSubtaskTitle(parent_id)`: calls `Core::create_task`
/// with `parent_id: Some(parent_id)`. On success, refreshes `app.rows` from
/// the full tree (so the new subtask lands nested under its parent) and
/// selects it — any error `refresh_rows_from_tree` itself sets is
/// preserved, not immediately cleared, since the task already committed. If
/// the parent has an assignee or either date set, enters `Mode::Confirm`
/// prompting whether to inherit those onto the new subtask; otherwise
/// returns straight to `Mode::Normal`. A genuine backend failure on that
/// follow-up `Core::get_task(parent_id)` fetch is surfaced via `app.error`
/// (rather than silently treated the same as "parent has nothing to
/// inherit"), but still falls through to `Mode::Normal` since the subtask
/// itself was created successfully. On `create_task` failure (e.g. an empty
/// title) sets `app.error` and stays in `Mode::Insert` with the buffer
/// intact, matching `submit_new_title`'s failure handling.
fn submit_new_subtask<S: Store>(
    app: &mut App,
    core: &mut Core<S>,
    parent_id: TaskId,
    buffer: String,
) {
    let new_task = NewTask {
        title: buffer,
        description: None,
        parent_id: Some(parent_id),
        type_key: None,
        start_date: None,
        due_date: None,
        duration_days: None,
        assignee_id: None,
    };

    match core.create_task(new_task) {
        Ok(task) => {
            let child_id = task.id;
            app.error = None;
            refresh_rows_from_tree(app, core, Some(child_id));

            match core.get_task(parent_id) {
                Ok(Some(parent))
                    if parent.assignee_id.is_some()
                        || parent.start_date.is_some()
                        || parent.due_date.is_some() =>
                {
                    app.mode = Mode::Confirm {
                        prompt: format!(
                            "Inherit assignee/dates from parent \"{}\"? (y/n)",
                            parent.title
                        ),
                        action: PendingAction::InheritFromParent {
                            child: child_id,
                            parent: parent_id,
                        },
                    };
                }
                Ok(_) => {
                    app.mode = Mode::Normal;
                }
                Err(err) => {
                    app.error = Some(err.to_string());
                    app.mode = Mode::Normal;
                }
            }
        }
        Err(err) => {
            app.error = Some(err.to_string());
        }
    }
}

/// `EditableField::NewTitle`: calls `Core::create_task` with `buffer` as the
/// title. On success appends+selects the new row; on failure (e.g. an empty
/// title) sets `app.error`. Any error `refresh_rows_from_tree` itself sets
/// on the success path is preserved, not immediately cleared, since the
/// task already committed.
fn submit_new_title<S: Store>(app: &mut App, core: &mut Core<S>, buffer: String) {
    let new_task = NewTask {
        title: buffer,
        description: None,
        parent_id: None,
        type_key: None,
        start_date: None,
        due_date: None,
        duration_days: None,
        assignee_id: None,
    };

    match core.create_task(new_task) {
        Ok(task) => {
            app.error = None;
            refresh_rows_from_tree(app, core, Some(task.id));
            app.mode = Mode::Normal;
        }
        Err(err) => {
            app.error = Some(err.to_string());
        }
    }
}

/// `EditableField::Title(id)`: calls `Core::update_task` with `buffer` as
/// the new title. On success refreshes the displayed title on the task's
/// row and on the cached task; on failure (e.g. an empty title) sets
/// `app.error`.
fn submit_edit_title<S: Store>(app: &mut App, core: &mut Core<S>, id: TaskId, buffer: String) {
    let patch = TaskPatch {
        title: Field::Set(buffer),
        ..Default::default()
    };
    match core.update_task(id, patch) {
        Ok(tasks) => {
            if let Some(updated) = tasks.iter().find(|task| task.id == id) {
                if let Some(row) = app.rows.iter_mut().find(|row| row.id == id) {
                    row.title.clone_from(&updated.title);
                }
                if let Some(cached) = app.tasks.iter_mut().find(|cached| cached.id == id) {
                    cached.title.clone_from(&updated.title);
                }
            }
            app.mode = Mode::Normal;
            app.error = None;
        }
        Err(err) => {
            app.error = Some(err.to_string());
        }
    }
}

/// `EditableField::Description(id)`: calls `Core::update_task` with
/// `buffer` as the new description. An empty buffer is not an error (unlike
/// title) — it simply sets an empty description. On success refreshes the
/// cached description; on failure sets `app.error`.
fn submit_edit_description<S: Store>(
    app: &mut App,
    core: &mut Core<S>,
    id: TaskId,
    buffer: String,
) {
    let patch = TaskPatch {
        description: Field::Set(buffer),
        ..Default::default()
    };
    match core.update_task(id, patch) {
        Ok(tasks) => {
            if let Some(updated) = tasks.iter().find(|task| task.id == id) {
                app.descriptions.insert(id, updated.description.clone());
            }
            app.mode = Mode::Normal;
            app.error = None;
        }
        Err(err) => {
            app.error = Some(err.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::ops::ControlFlow;

    use bala_core::{Core, DeleteMode, InMemoryStore, TaskId, TaskStatus, TaskType};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{App, apply_action, handle_key};
    use crate::render::TaskRow;
    use crate::tui::keymap::Action;
    use crate::tui::mode::{DetailField, EditableField, Mode, Pane, PendingAction};

    fn row(title: &str) -> TaskRow {
        TaskRow {
            id: TaskId::new(),
            title: title.to_string(),
            type_label: "task".to_string(),
            status: TaskStatus::Incomplete,
            assignee_name: None,
            depth: 0,
            has_children: false,
            collapsed: false,
            direct_summary: None,
            parent_id: None,
            blocked: false,
        }
    }

    fn core() -> Core<InMemoryStore> {
        Core::new(InMemoryStore::default()).expect("in-memory core should construct")
    }

    #[test]
    fn app_new_should_select_first_row_when_rows_present() {
        let row_a = row("First");
        let row_b = row("Second");
        let app = App::new(vec![row_a.clone(), row_b]);

        assert_eq!(app.selected_row(), Some(&row_a));
    }

    #[test]
    fn app_new_should_have_no_selection_when_rows_empty() {
        let app = App::new(vec![]);

        assert_eq!(app.selected_row(), None);
    }

    #[test]
    fn move_down_should_advance_selection() {
        let row_a = row("First");
        let row_b = row("Second");
        let mut app = App::new(vec![row_a, row_b.clone()]);

        app.move_down();

        assert_eq!(app.selected_row(), Some(&row_b));
    }

    #[test]
    fn move_down_at_last_row_should_clamp() {
        let row_a = row("First");
        let row_b = row("Second");
        let mut app = App::new(vec![row_a, row_b.clone()]);

        app.move_down();
        app.move_down();

        assert_eq!(app.selected_row(), Some(&row_b));
    }

    #[test]
    fn move_up_at_first_row_should_clamp() {
        let row_a = row("First");
        let row_b = row("Second");
        let mut app = App::new(vec![row_a.clone(), row_b]);

        app.move_up();

        assert_eq!(app.selected_row(), Some(&row_a));
    }

    #[test]
    fn move_up_and_move_down_should_be_noop_when_rows_empty() {
        let mut app = App::new(vec![]);

        app.move_up();
        app.move_down();

        assert_eq!(app.selected_row(), None);
    }

    #[test]
    fn handle_key_j_and_down_arrow_should_move_selection_down() {
        let mut app = App::new(vec![row("First"), row("Second"), row("Third")]);
        let mut core = core();

        let result = handle_key(
            &mut app,
            &mut core,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        );
        assert_eq!(result, ControlFlow::Continue(()));
        assert_eq!(app.selected_index(), Some(1));

        let result = handle_key(
            &mut app,
            &mut core,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        );
        assert_eq!(result, ControlFlow::Continue(()));
        assert_eq!(app.selected_index(), Some(2));
    }

    #[test]
    fn handle_key_k_and_up_arrow_should_move_selection_up() {
        let mut app = App::new(vec![row("First"), row("Second"), row("Third")]);
        let mut core = core();
        app.move_down();
        app.move_down();
        assert_eq!(app.selected_index(), Some(2));

        let result = handle_key(
            &mut app,
            &mut core,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
        );
        assert_eq!(result, ControlFlow::Continue(()));
        assert_eq!(app.selected_index(), Some(1));

        let result = handle_key(
            &mut app,
            &mut core,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        );
        assert_eq!(result, ControlFlow::Continue(()));
        assert_eq!(app.selected_index(), Some(0));
    }

    #[test]
    fn handle_key_q_should_signal_quit() {
        let mut app = App::new(vec![row("First"), row("Second")]);
        let mut core = core();

        let result = handle_key(
            &mut app,
            &mut core,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );

        assert_eq!(result, ControlFlow::Break(()));
        // Quitting doesn't mutate selection.
        assert_eq!(app.selected_index(), Some(0));
    }

    #[test]
    fn handle_key_unmapped_key_should_be_noop_and_continue() {
        let mut app = App::new(vec![row("First"), row("Second")]);
        let mut core = core();

        let result = handle_key(
            &mut app,
            &mut core,
            KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE),
        );

        assert_eq!(result, ControlFlow::Continue(()));
        assert_eq!(app.selected_index(), Some(0));
    }

    #[test]
    fn apply_action_start_insert_new_title_should_enter_insert_mode_with_empty_buffer() {
        let mut app = App::new(vec![]);
        let mut core = core();

        let _ = apply_action(&mut app, &mut core, Action::StartInsertNewTitle);

        assert_eq!(
            app.mode(),
            &Mode::Insert {
                field: EditableField::NewTitle,
                buffer: String::new(),
            }
        );
    }

    #[test]
    fn apply_action_insert_char_should_append_to_buffer() {
        let mut app = App::new(vec![]);
        let mut core = core();
        let _ = apply_action(&mut app, &mut core, Action::StartInsertNewTitle);

        let _ = apply_action(&mut app, &mut core, Action::InsertChar('H'));
        let _ = apply_action(&mut app, &mut core, Action::InsertChar('i'));

        assert_eq!(
            app.mode(),
            &Mode::Insert {
                field: EditableField::NewTitle,
                buffer: "Hi".to_string(),
            }
        );
    }

    #[test]
    fn apply_action_backspace_should_remove_last_buffer_char() {
        let mut app = App::new(vec![]);
        let mut core = core();
        let _ = apply_action(&mut app, &mut core, Action::StartInsertNewTitle);
        let _ = apply_action(&mut app, &mut core, Action::InsertChar('H'));
        let _ = apply_action(&mut app, &mut core, Action::InsertChar('i'));

        let _ = apply_action(&mut app, &mut core, Action::Backspace);

        assert_eq!(
            app.mode(),
            &Mode::Insert {
                field: EditableField::NewTitle,
                buffer: "H".to_string(),
            }
        );
    }

    #[test]
    fn apply_action_cancel_insert_should_return_to_normal_without_calling_create_task() {
        let mut app = App::new(vec![]);
        let mut core = core();
        let _ = apply_action(&mut app, &mut core, Action::StartInsertNewTitle);
        let _ = apply_action(&mut app, &mut core, Action::InsertChar('H'));

        let _ = apply_action(&mut app, &mut core, Action::CancelInsert);

        assert_eq!(app.mode(), &Mode::Normal);
        assert_eq!(app.rows(), []);
    }

    #[test]
    fn apply_action_submit_insert_new_title_should_call_create_task_and_add_selected_row() {
        let mut app = App::new(vec![]);
        let mut core = core();
        let _ = apply_action(&mut app, &mut core, Action::StartInsertNewTitle);
        for c in "Write docs".chars() {
            let _ = apply_action(&mut app, &mut core, Action::InsertChar(c));
        }

        let _ = apply_action(&mut app, &mut core, Action::SubmitInsert);

        assert_eq!(app.mode(), &Mode::Normal);
        assert_eq!(app.rows().len(), 1);
        assert_eq!(app.rows()[0].title, "Write docs");
        assert_eq!(app.selected_index(), Some(0));
        assert_eq!(app.error(), None);
    }

    #[test]
    fn apply_action_submit_insert_new_title_with_empty_buffer_should_show_inline_error_and_stay_in_insert_mode()
     {
        let mut app = App::new(vec![]);
        let mut core = core();
        let _ = apply_action(&mut app, &mut core, Action::StartInsertNewTitle);

        let _ = apply_action(&mut app, &mut core, Action::SubmitInsert);

        assert!(matches!(
            app.mode(),
            Mode::Insert {
                field: EditableField::NewTitle,
                buffer,
            } if buffer.is_empty()
        ));
        assert!(app.error().is_some());
        assert_eq!(app.rows(), []);
    }

    #[test]
    fn apply_action_enter_detail_should_switch_pane_and_default_cursor_to_title() {
        let mut app = App::new(vec![row("First")]);
        let mut core = core();
        let _ = apply_action(&mut app, &mut core, Action::DetailCursorDown);

        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);

        assert_eq!(app.pane(), Pane::Detail);
        assert_eq!(app.detail_field(), DetailField::Title);
    }

    #[test]
    fn apply_action_enter_detail_should_be_noop_when_no_row_is_selected() {
        let mut app = App::new(vec![]);
        let mut core = core();

        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);

        assert_eq!(app.pane(), Pane::List);
    }

    #[test]
    fn apply_action_leave_detail_should_switch_back_to_list_pane() {
        let mut app = App::new(vec![row("First")]);
        let mut core = core();
        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);

        let _ = apply_action(&mut app, &mut core, Action::LeaveDetail);

        assert_eq!(app.pane(), Pane::List);
    }

    #[test]
    fn apply_action_start_edit_title_should_prefill_buffer_with_current_title() {
        let task_row = row("Original title");
        let id = task_row.id;
        let mut app = App::new(vec![task_row]);
        let mut core = core();
        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);

        let _ = apply_action(&mut app, &mut core, Action::StartEditTitle);

        assert_eq!(
            app.mode(),
            &Mode::Insert {
                field: EditableField::Title(id),
                buffer: "Original title".to_string(),
            }
        );
    }

    #[test]
    fn apply_action_start_edit_description_should_prefill_buffer_with_empty_string_when_none() {
        let task_row = row("First");
        let id = task_row.id;
        let mut app = App::new(vec![task_row]);
        let mut core = core();
        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);
        let _ = apply_action(&mut app, &mut core, Action::DetailCursorDown);

        let _ = apply_action(&mut app, &mut core, Action::StartEditDescription);

        assert_eq!(
            app.mode(),
            &Mode::Insert {
                field: EditableField::Description(id),
                buffer: String::new(),
            }
        );
    }

    #[test]
    fn apply_action_submit_insert_edit_title_should_call_update_task_and_refresh_row() {
        let mut core = core();
        let task = core
            .create_task(bala_core::NewTask {
                title: "Old title".to_string(),
                description: None,
                parent_id: None,
                type_key: None,
                start_date: None,
                due_date: None,
                duration_days: None,
                assignee_id: None,
            })
            .expect("create_task should succeed");
        let mut app = App::new(vec![row_for(&task)]);
        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);
        let _ = apply_action(&mut app, &mut core, Action::StartEditTitle);
        for _ in 0.."Old title".chars().count() {
            let _ = apply_action(&mut app, &mut core, Action::Backspace);
        }
        for c in "New title".chars() {
            let _ = apply_action(&mut app, &mut core, Action::InsertChar(c));
        }

        let _ = apply_action(&mut app, &mut core, Action::SubmitInsert);

        assert_eq!(app.mode(), &Mode::Normal);
        assert_eq!(app.pane(), Pane::Detail);
        assert_eq!(app.rows()[0].title, "New title");
        assert_eq!(app.error(), None);
    }

    #[test]
    fn apply_action_submit_insert_edit_title_with_empty_buffer_should_show_inline_error_and_stay_in_insert_mode()
     {
        let mut core = core();
        let task = core
            .create_task(bala_core::NewTask {
                title: "Old title".to_string(),
                description: None,
                parent_id: None,
                type_key: None,
                start_date: None,
                due_date: None,
                duration_days: None,
                assignee_id: None,
            })
            .expect("create_task should succeed");
        let id = task.id;
        let mut app = App::new(vec![row_for(&task)]);
        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);
        let _ = apply_action(&mut app, &mut core, Action::StartEditTitle);
        for _ in 0.."Old title".chars().count() {
            let _ = apply_action(&mut app, &mut core, Action::Backspace);
        }

        let _ = apply_action(&mut app, &mut core, Action::SubmitInsert);

        assert!(matches!(
            app.mode(),
            Mode::Insert {
                field: EditableField::Title(edited_id),
                buffer,
            } if *edited_id == id && buffer.is_empty()
        ));
        assert!(app.error().is_some());
        assert_eq!(app.rows()[0].title, "Old title");
    }

    #[test]
    fn apply_action_submit_insert_edit_description_should_call_update_task_with_description_patch()
    {
        let mut core = core();
        let task = core
            .create_task(bala_core::NewTask {
                title: "Task".to_string(),
                description: None,
                parent_id: None,
                type_key: None,
                start_date: None,
                due_date: None,
                duration_days: None,
                assignee_id: None,
            })
            .expect("create_task should succeed");
        let id = task.id;
        let mut app = App::new(vec![row_for(&task)]);
        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);
        let _ = apply_action(&mut app, &mut core, Action::DetailCursorDown);
        let _ = apply_action(&mut app, &mut core, Action::StartEditDescription);
        for c in "New description".chars() {
            let _ = apply_action(&mut app, &mut core, Action::InsertChar(c));
        }

        let _ = apply_action(&mut app, &mut core, Action::SubmitInsert);

        assert_eq!(app.mode(), &Mode::Normal);
        assert_eq!(app.pane(), Pane::Detail);
        assert_eq!(app.error(), None);
        let tasks = core
            .get_tree(bala_core::TreeFilter::default())
            .expect("get_tree should succeed");
        let updated = tasks
            .into_iter()
            .find(|task| task.id == id)
            .expect("task should still exist");
        assert_eq!(updated.description.as_deref(), Some("New description"));
    }

    #[test]
    fn apply_action_cancel_insert_edit_should_return_to_detail_pane_without_calling_update_task() {
        let mut core = core();
        let task = core
            .create_task(bala_core::NewTask {
                title: "Old title".to_string(),
                description: None,
                parent_id: None,
                type_key: None,
                start_date: None,
                due_date: None,
                duration_days: None,
                assignee_id: None,
            })
            .expect("create_task should succeed");
        let mut app = App::new(vec![row_for(&task)]);
        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);
        let _ = apply_action(&mut app, &mut core, Action::StartEditTitle);
        let _ = apply_action(&mut app, &mut core, Action::InsertChar('!'));

        let _ = apply_action(&mut app, &mut core, Action::CancelInsert);

        assert_eq!(app.mode(), &Mode::Normal);
        assert_eq!(app.pane(), Pane::Detail);
        assert_eq!(app.rows()[0].title, "Old title");
    }

    #[test]
    fn apply_action_single_d_key_pressed_should_not_change_mode() {
        let mut app = App::new(vec![row("First")]);
        let mut core = core();

        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);

        assert_eq!(app.mode(), &Mode::Normal);
    }

    #[test]
    fn apply_action_two_consecutive_d_key_presses_should_enter_confirm_mode_with_delete_prompt() {
        let task_row = row("Write docs");
        let id = task_row.id;
        let mut app = App::new(vec![task_row]);
        let mut core = core();
        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);

        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);

        match app.mode() {
            Mode::Confirm { prompt, action } => {
                assert!(prompt.contains("Write docs"));
                assert_eq!(*action, crate::tui::mode::PendingAction::Delete(id));
            }
            other => panic!("expected Mode::Confirm, got {other:?}"),
        }
    }

    #[test]
    fn apply_action_non_d_action_between_d_presses_should_reset_pending_delete() {
        let mut app = App::new(vec![row("First"), row("Second")]);
        let mut core = core();
        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);

        let _ = apply_action(&mut app, &mut core, Action::MoveDown);
        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);

        assert_eq!(app.mode(), &Mode::Normal);
    }

    #[test]
    fn apply_action_d_key_pressed_twice_with_no_selection_should_be_noop() {
        let mut app = App::new(vec![]);
        let mut core = core();

        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);
        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);

        assert_eq!(app.mode(), &Mode::Normal);
    }

    #[test]
    fn apply_action_open_help_should_enter_help_mode_remembering_normal_as_previous() {
        let mut app = App::new(vec![row("First")]);
        let mut core = core();

        let _ = apply_action(&mut app, &mut core, Action::OpenHelp);

        match app.mode() {
            Mode::Help { previous } => assert_eq!(**previous, Mode::Normal),
            other => panic!("expected Mode::Help, got {other:?}"),
        }
    }

    #[test]
    fn apply_action_open_help_from_confirm_mode_should_remember_confirm_as_previous() {
        let task_row = row("Write docs");
        let id = task_row.id;
        let mut app = App::new(vec![task_row]);
        let mut core = core();
        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);
        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);
        let confirm_mode = app.mode().clone();
        assert!(matches!(confirm_mode, Mode::Confirm { .. }));

        let _ = apply_action(&mut app, &mut core, Action::OpenHelp);

        match app.mode() {
            Mode::Help { previous } => {
                assert_eq!(**previous, confirm_mode);
                match previous.as_ref() {
                    Mode::Confirm { action, .. } => {
                        assert_eq!(*action, crate::tui::mode::PendingAction::Delete(id));
                    }
                    other => panic!("expected Mode::Confirm, got {other:?}"),
                }
            }
            other => panic!("expected Mode::Help, got {other:?}"),
        }
    }

    #[test]
    fn apply_action_close_help_should_restore_the_previous_mode() {
        let mut app = App::new(vec![row("First")]);
        let mut core = core();
        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);
        let _ = apply_action(&mut app, &mut core, Action::OpenHelp);

        let _ = apply_action(&mut app, &mut core, Action::CloseHelp);

        assert_eq!(app.mode(), &Mode::Normal);
        assert_eq!(app.pane(), Pane::Detail);
    }

    #[test]
    fn apply_action_close_help_opened_mid_insert_should_preserve_the_in_progress_buffer() {
        let mut app = App::new(vec![row("First")]);
        let mut core = core();
        let _ = apply_action(&mut app, &mut core, Action::StartInsertNewTitle);
        let _ = apply_action(&mut app, &mut core, Action::InsertChar('H'));
        let _ = apply_action(&mut app, &mut core, Action::InsertChar('i'));

        let _ = apply_action(&mut app, &mut core, Action::OpenHelp);
        let _ = apply_action(&mut app, &mut core, Action::CloseHelp);

        match app.mode() {
            Mode::Insert { field, buffer } => {
                assert_eq!(*field, EditableField::NewTitle);
                assert_eq!(buffer, "Hi");
            }
            other => panic!("expected Mode::Insert with buffer preserved, got {other:?}"),
        }
    }

    #[test]
    fn apply_action_confirm_yes_should_call_delete_task_with_subtree_mode_and_remove_row() {
        let mut core = core();
        let task = core
            .create_task(bala_core::NewTask {
                title: "Write docs".to_string(),
                description: None,
                parent_id: None,
                type_key: None,
                start_date: None,
                due_date: None,
                duration_days: None,
                assignee_id: None,
            })
            .expect("create_task should succeed");
        let id = task.id;
        let mut app = App::new(vec![row_for(&task)]);
        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);
        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);

        let _ = apply_action(&mut app, &mut core, Action::ConfirmYes);

        assert_eq!(app.mode(), &Mode::Normal);
        assert_eq!(app.pane(), Pane::List);
        assert_eq!(app.rows(), []);
        let tasks = core
            .get_tree(bala_core::TreeFilter::default())
            .expect("get_tree should succeed");
        assert!(!tasks.iter().any(|task| task.id == id));
    }

    #[test]
    fn apply_action_confirm_yes_delete_should_clear_has_children_when_parent_loses_its_only_child()
    {
        let mut core = core();
        let p = core.create_task(minimal_new_task("P")).expect("create");
        let k = child_of(&mut core, "K", p.id);
        let mut app = app_from_core(&mut core);
        app.select_by_id(Some(k.id));
        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);
        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);

        let _ = apply_action(&mut app, &mut core, Action::ConfirmYes);

        assert_eq!(app.error(), None);
        assert_eq!(titles(&app), vec!["P"]);
        assert!(!app.rows()[0].has_children);
        assert_eq!(app.selected_row().map(|r| r.id), Some(p.id));
    }

    /// A [`bala_core::Store`] over an [`InMemoryStore`] that fails every transaction
    /// once its shared budget of successful ones runs out (`None` = no
    /// limit), so a test can let a mutation commit and then fail the
    /// follow-up fetch.
    struct FlakyStore {
        inner: InMemoryStore,
        remaining: std::rc::Rc<std::cell::Cell<Option<usize>>>,
    }

    impl bala_core::Store for FlakyStore {
        fn transaction<T>(
            &self,
            f: impl FnOnce(&mut dyn bala_core::StoreTx) -> Result<T, bala_core::StoreError>,
        ) -> Result<T, bala_core::StoreError> {
            match self.remaining.get() {
                Some(0) => Err(bala_core::StoreError::Backend("flaky".to_string())),
                Some(n) => {
                    self.remaining.set(Some(n - 1));
                    self.inner.transaction(f)
                }
                None => self.inner.transaction(f),
            }
        }
    }

    #[test]
    fn apply_action_confirm_delete_subtree_should_drop_deleted_rows_when_refresh_fails() {
        let remaining = std::rc::Rc::new(std::cell::Cell::new(None));
        let mut core = Core::new(FlakyStore {
            inner: InMemoryStore::default(),
            remaining: std::rc::Rc::clone(&remaining),
        })
        .expect("core should construct");
        let a = core.create_task(minimal_new_task("A")).expect("create");
        let b = core.create_task(minimal_new_task("B")).expect("create");
        let c = core
            .create_task(bala_core::NewTask {
                parent_id: Some(b.id),
                ..minimal_new_task("C")
            })
            .expect("create");
        let tasks = core
            .get_tree(bala_core::TreeFilter::default())
            .expect("get_tree should succeed");
        let order = core.sibling_order().expect("sibling_order should succeed");
        let rows = crate::render::task_rows(
            &tasks,
            &order,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        );
        let mut app = App::new(rows).with_tasks(tasks).with_sibling_order(order);
        app.select_by_id(Some(a.id));
        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);
        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);
        // The delete commits; the refresh's `get_tree` then fails.
        remaining.set(Some(1));

        let _ = apply_action(
            &mut app,
            &mut core,
            Action::ConfirmDelete(DeleteMode::Subtree),
        );

        assert!(app.error().is_some());
        assert!(!app.rows().iter().any(|row| row.id == a.id));
        assert!(!app.tasks.iter().any(|task| task.id == a.id));
        assert!(app.rows().iter().any(|row| row.id == c.id));
        let selected = app.selected.expect("a row should stay selected");
        assert!(selected < app.rows().len());
        assert_eq!(app.mode(), &Mode::Normal);
    }

    #[test]
    fn toggle_complete_should_keep_the_error_when_the_tree_refresh_fails() {
        let remaining = std::rc::Rc::new(std::cell::Cell::new(None));
        let mut core = Core::new(FlakyStore {
            inner: InMemoryStore::default(),
            remaining: std::rc::Rc::clone(&remaining),
        })
        .expect("core should construct");
        let a = core.create_task(minimal_new_task("A")).expect("create");
        let tasks = core
            .get_tree(bala_core::TreeFilter::default())
            .expect("get_tree should succeed");
        let order = core.sibling_order().expect("sibling_order should succeed");
        let rows = crate::render::task_rows(
            &tasks,
            &order,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        );
        let mut app = App::new(rows).with_tasks(tasks).with_sibling_order(order);
        app.select_by_id(Some(a.id));
        // The completion commits; the tree re-read then fails.
        remaining.set(Some(1));

        let _ = apply_action(&mut app, &mut core, Action::ToggleComplete);

        assert!(app.error().is_some());
    }

    #[test]
    fn reload_predecessors_should_not_replace_an_error_already_shown() {
        let remaining = std::rc::Rc::new(std::cell::Cell::new(None));
        let mut core = Core::new(FlakyStore {
            inner: InMemoryStore::default(),
            remaining: std::rc::Rc::clone(&remaining),
        })
        .expect("core should construct");
        let pred = core.create_task(minimal_new_task("Pred")).expect("create");
        let blocked = core
            .create_task(minimal_new_task("Blocked"))
            .expect("create");
        core.add_dependency(blocked.id, pred.id, bala_core::DependencyType::default())
            .expect("add dependency");
        let tasks = core
            .get_tree(bala_core::TreeFilter::default())
            .expect("get_tree should succeed");
        let order = core.sibling_order().expect("sibling_order should succeed");
        let rows = crate::render::task_rows(
            &tasks,
            &order,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        );
        let mut app = App::new(rows).with_tasks(tasks).with_sibling_order(order);
        app.select_by_id(Some(blocked.id));
        // Predecessors are only read while the Detail pane is showing.
        app.pane = Pane::Detail;
        app.error = Some("original".to_string());
        remaining.set(Some(0));

        super::reload_predecessors(&mut app, &core);

        assert_eq!(app.error(), Some("original"));
    }

    #[test]
    fn apply_action_insert_char_should_not_reload_predecessors() {
        let remaining = std::rc::Rc::new(std::cell::Cell::new(None));
        let mut core = Core::new(FlakyStore {
            inner: InMemoryStore::default(),
            remaining: std::rc::Rc::clone(&remaining),
        })
        .expect("core should construct");
        let pred = core.create_task(minimal_new_task("Pred")).expect("create");
        let blocked = core
            .create_task(minimal_new_task("Blocked"))
            .expect("create");
        core.add_dependency(blocked.id, pred.id, bala_core::DependencyType::default())
            .expect("add dependency");
        let tasks = core
            .get_tree(bala_core::TreeFilter::default())
            .expect("get_tree should succeed");
        let order = core.sibling_order().expect("sibling_order should succeed");
        let rows = crate::render::task_rows(
            &tasks,
            &order,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        );
        let mut app = App::new(rows).with_tasks(tasks).with_sibling_order(order);
        app.select_by_id(Some(blocked.id));
        // Predecessors are only read while the Detail pane is showing.
        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);
        let _ = apply_action(&mut app, &mut core, Action::StartInsertNewTitle);
        remaining.set(Some(0));

        let _ = apply_action(&mut app, &mut core, Action::InsertChar('x'));

        assert_eq!(app.error(), None);
    }

    #[test]
    fn apply_action_confirm_no_should_return_to_normal_without_calling_delete_task() {
        let mut core = core();
        let task = core
            .create_task(bala_core::NewTask {
                title: "Write docs".to_string(),
                description: None,
                parent_id: None,
                type_key: None,
                start_date: None,
                due_date: None,
                duration_days: None,
                assignee_id: None,
            })
            .expect("create_task should succeed");
        let id = task.id;
        let mut app = App::new(vec![row_for(&task)]);
        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);
        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);

        let _ = apply_action(&mut app, &mut core, Action::ConfirmNo);

        assert_eq!(app.mode(), &Mode::Normal);
        assert_eq!(app.rows().len(), 1);
        let tasks = core
            .get_tree(bala_core::TreeFilter::default())
            .expect("get_tree should succeed");
        assert!(tasks.iter().any(|task| task.id == id));
    }

    /// Presses `d` twice with `id`'s first row selected.
    fn press_dd_on(app: &mut App, core: &mut Core<InMemoryStore>, id: TaskId) {
        app.select_by_id(Some(id));
        let _ = apply_action(app, core, Action::DKeyPressed);
        let _ = apply_action(app, core, Action::DKeyPressed);
    }

    fn live_tasks(core: &Core<InMemoryStore>) -> Vec<bala_core::Task> {
        core.get_tree(bala_core::TreeFilter::default())
            .expect("get_tree should succeed")
    }

    #[test]
    fn dd_on_task_with_children_should_enter_confirm_not_delete_immediately() {
        let mut core = core();
        let p = core.create_task(minimal_new_task("P")).expect("create");
        let k1 = child_of(&mut core, "K1", p.id);
        let k2 = child_of(&mut core, "K2", p.id);
        let mut app = app_from_core(&mut core);

        press_dd_on(&mut app, &mut core, p.id);

        match app.mode() {
            Mode::Confirm { prompt, action } => {
                assert_eq!(
                    prompt,
                    "\"P\" has 2 subtask(s). Delete [s]ubtree, [p]romote children, or [n] cancel?"
                );
                assert_eq!(*action, PendingAction::DeleteWithChildren(p.id));
            }
            other => panic!("expected Mode::Confirm, got {other:?}"),
        }
        let ids: Vec<TaskId> = live_tasks(&core).iter().map(|t| t.id).collect();
        assert!(ids.contains(&p.id) && ids.contains(&k1.id) && ids.contains(&k2.id));

        // `y` is not an answer to this prompt.
        let _ = handle_key(
            &mut app,
            &mut core,
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
        );
        assert!(matches!(app.mode(), Mode::Confirm { .. }));
        assert!(live_tasks(&core).iter().any(|t| t.id == p.id));
    }

    #[test]
    fn dd_on_leaf_should_enter_plain_delete_confirm() {
        let mut core = core();
        let p = core.create_task(minimal_new_task("P")).expect("create");
        let k = child_of(&mut core, "K", p.id);
        let mut app = app_from_core(&mut core);

        press_dd_on(&mut app, &mut core, k.id);

        assert_eq!(
            app.mode(),
            &Mode::Confirm {
                prompt: "Delete \"K\"? (y/n)".to_string(),
                action: PendingAction::Delete(k.id),
            }
        );
    }

    #[test]
    fn confirm_yes_on_leaf_prompt_should_keep_child_added_after_prompt_opened() {
        let mut core = core();
        let t = core.create_task(minimal_new_task("T")).expect("create");
        let mut app = app_from_core(&mut core);
        press_dd_on(&mut app, &mut core, t.id);
        assert_eq!(
            app.mode(),
            &Mode::Confirm {
                prompt: "Delete \"T\"? (y/n)".to_string(),
                action: PendingAction::Delete(t.id),
            }
        );
        // Another writer sharing the store adds a child while the prompt is open.
        let k = child_of(&mut core, "K", t.id);

        let _ = handle_key(
            &mut app,
            &mut core,
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
        );

        assert_eq!(app.error(), None);
        let tasks = live_tasks(&core);
        assert!(!tasks.iter().any(|task| task.id == t.id));
        let kid = tasks
            .iter()
            .find(|task| task.id == k.id)
            .expect("a child the prompt never mentioned should survive");
        assert_eq!(kid.parent_id, None);
        assert!(app.rows().iter().any(|row| row.id == k.id));
    }

    #[test]
    fn dd_on_parent_whose_children_are_filtered_out_should_still_offer_delete_mode_choice() {
        let mut core = core();
        core.upsert_task_type(TaskType {
            key: "goal".to_string(),
            label: "Goal".to_string(),
            color: None,
            sort_order: 1,
        })
        .expect("upsert_task_type should succeed");
        let p = core
            .create_task(bala_core::NewTask {
                type_key: Some("goal".to_string()),
                ..minimal_new_task("P")
            })
            .expect("create");
        let _k = child_of(&mut core, "K", p.id);
        let mut app = App::new(vec![])
            .with_type_filter_state(Some("goal".to_string()), vec!["goal".to_string()]);
        super::refresh_rows_from_tree(&mut app, &mut core, None);
        assert_eq!(titles(&app), vec!["P"]);

        press_dd_on(&mut app, &mut core, p.id);

        match app.mode() {
            Mode::Confirm { prompt, action } => {
                assert!(prompt.contains("has 1 subtask(s)"), "prompt: {prompt}");
                assert_eq!(*action, PendingAction::DeleteWithChildren(p.id));
            }
            other => panic!("expected Mode::Confirm, got {other:?}"),
        }
    }

    #[test]
    fn confirm_delete_subtree_should_delete_parent_and_its_children() {
        let mut core = core();
        let p = core.create_task(minimal_new_task("P")).expect("create");
        let k = child_of(&mut core, "K", p.id);
        let other = core.create_task(minimal_new_task("Other")).expect("create");
        let mut app = app_from_core(&mut core);
        press_dd_on(&mut app, &mut core, p.id);

        let _ = apply_action(
            &mut app,
            &mut core,
            Action::ConfirmDelete(DeleteMode::Subtree),
        );

        assert_eq!(app.error(), None);
        assert_eq!(app.mode(), &Mode::Normal);
        assert_eq!(app.pane(), Pane::List);
        assert_eq!(titles(&app), vec!["Other"]);
        let ids: Vec<TaskId> = live_tasks(&core).iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![other.id]);
        assert!(!ids.contains(&k.id));
    }

    #[test]
    fn confirm_delete_promote_should_reparent_children_to_deleted_tasks_parent_and_show_them() {
        let mut core = core();
        let g = core.create_task(minimal_new_task("G")).expect("create");
        let p = child_of(&mut core, "P", g.id);
        let k1 = child_of(&mut core, "K1", p.id);
        let k2 = child_of(&mut core, "K2", p.id);
        let mut app = app_from_core(&mut core);
        press_dd_on(&mut app, &mut core, p.id);

        let _ = apply_action(
            &mut app,
            &mut core,
            Action::ConfirmDelete(DeleteMode::PromoteChildren),
        );

        assert_eq!(app.error(), None);
        assert_eq!(app.mode(), &Mode::Normal);
        let tasks = live_tasks(&core);
        assert!(!tasks.iter().any(|t| t.id == p.id));
        for kid in [k1.id, k2.id] {
            let task = tasks.iter().find(|t| t.id == kid).expect("child survives");
            assert_eq!(task.parent_id, Some(g.id));
        }
        assert!(!app.rows().iter().any(|r| r.id == p.id));
        for kid in [k1.id, k2.id] {
            let row = app
                .rows()
                .iter()
                .find(|r| r.id == kid)
                .expect("promoted child should be visible");
            assert_eq!(row.parent_id, Some(g.id));
            assert_eq!(row.depth, 1);
        }
        let mut shown = titles(&app);
        shown.sort();
        assert_eq!(shown, vec!["G", "K1", "K2"]);
    }

    #[test]
    fn confirm_delete_promote_on_top_level_parent_should_make_children_top_level() {
        let mut core = core();
        let p = core.create_task(minimal_new_task("P")).expect("create");
        let k = child_of(&mut core, "K", p.id);
        let mut app = app_from_core(&mut core);
        press_dd_on(&mut app, &mut core, p.id);

        let _ = apply_action(
            &mut app,
            &mut core,
            Action::ConfirmDelete(DeleteMode::PromoteChildren),
        );

        assert_eq!(app.error(), None);
        let tasks = live_tasks(&core);
        let task = tasks.iter().find(|t| t.id == k.id).expect("child survives");
        assert_eq!(task.parent_id, None);
        assert_eq!(titles(&app), vec!["K"]);
        let row = &app.rows()[0];
        assert_eq!(row.id, k.id);
        assert_eq!(row.depth, 0);
        assert_eq!(row.parent_id, None);
        assert_eq!(app.selected_row().map(|r| r.id), Some(k.id));
    }

    #[test]
    fn confirm_no_on_delete_with_children_prompt_should_change_nothing() {
        for code in [KeyCode::Char('n'), KeyCode::Esc] {
            let mut core = core();
            let p = core.create_task(minimal_new_task("P")).expect("create");
            let k = child_of(&mut core, "K", p.id);
            let mut app = app_from_core(&mut core);
            press_dd_on(&mut app, &mut core, p.id);
            let rows_before = app.rows().to_vec();

            let _ = handle_key(&mut app, &mut core, KeyEvent::new(code, KeyModifiers::NONE));

            assert_eq!(app.mode(), &Mode::Normal, "{code:?}");
            assert_eq!(app.rows(), rows_before.as_slice(), "{code:?}");
            let tasks = live_tasks(&core);
            assert!(tasks.iter().any(|t| t.id == p.id), "{code:?}");
            let kid = tasks.iter().find(|t| t.id == k.id).expect("child survives");
            assert_eq!(kid.parent_id, Some(p.id), "{code:?}");
        }
    }

    #[test]
    fn dd_should_set_error_and_stay_normal_when_listing_children_fails() {
        let remaining = std::rc::Rc::new(std::cell::Cell::new(None));
        let mut core = Core::new(FlakyStore {
            inner: InMemoryStore::default(),
            remaining: std::rc::Rc::clone(&remaining),
        })
        .expect("core should construct");
        let p = core.create_task(minimal_new_task("P")).expect("create");
        let mut app = App::new(vec![row_for(&p)]);
        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);
        remaining.set(Some(0));

        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);

        assert_eq!(app.mode(), &Mode::Normal);
        assert!(app.error().is_some());
        remaining.set(None);
        assert!(
            core.get_tree(bala_core::TreeFilter::default())
                .expect("get_tree should succeed")
                .iter()
                .any(|t| t.id == p.id)
        );
    }

    /// The text of the open `Mode::Confirm` prompt.
    fn confirm_prompt(app: &App) -> &str {
        match app.mode() {
            Mode::Confirm { prompt, .. } => prompt,
            other => panic!("expected Mode::Confirm, got {other:?}"),
        }
    }

    #[test]
    fn dd_on_a_leaf_with_dependents_should_name_them_in_the_prompt() {
        let mut core = core();
        let a = core.create_task(minimal_new_task("A")).expect("create");
        let b = core.create_task(minimal_new_task("B")).expect("create");
        let c = core.create_task(minimal_new_task("C")).expect("create");
        depend_on(&mut core, b.id, a.id);
        depend_on(&mut core, c.id, a.id);
        let mut app = app_from_core(&mut core);

        press_dd_on(&mut app, &mut core, a.id);

        assert_eq!(
            app.mode(),
            &Mode::Confirm {
                prompt: "2 task(s) depend on this task:\n  B\n  C\nDelete \"A\"? (y/n)".to_string(),
                action: PendingAction::Delete(a.id),
            }
        );
    }

    #[test]
    fn dd_on_a_task_without_dependents_should_keep_the_one_line_prompt() {
        let mut core = core();
        let a = core.create_task(minimal_new_task("A")).expect("create");
        let b = core.create_task(minimal_new_task("B")).expect("create");
        let p = core.create_task(minimal_new_task("P")).expect("create");
        let _k = child_of(&mut core, "K", p.id);
        // `A` depends on `B`, but nothing depends on `A` or on `P`'s subtree.
        depend_on(&mut core, a.id, b.id);
        let mut app = app_from_core(&mut core);

        press_dd_on(&mut app, &mut core, a.id);
        assert_eq!(confirm_prompt(&app), "Delete \"A\"? (y/n)");
        let _ = apply_action(&mut app, &mut core, Action::ConfirmNo);

        press_dd_on(&mut app, &mut core, p.id);
        assert_eq!(
            confirm_prompt(&app),
            "\"P\" has 1 subtask(s). Delete [s]ubtree, [p]romote children, or [n] cancel?"
        );
    }

    #[test]
    fn dd_on_a_parent_should_name_dependents_of_its_descendants_as_subtree_only() {
        let mut core = core();
        let a = core.create_task(minimal_new_task("A")).expect("create");
        let k1 = child_of(&mut core, "K1", a.id);
        let _k2 = child_of(&mut core, "K2", a.id);
        let grandchild = child_of(&mut core, "G", k1.id);
        let b = core.create_task(minimal_new_task("B")).expect("create");
        let c = core.create_task(minimal_new_task("C")).expect("create");
        let d = core.create_task(minimal_new_task("D")).expect("create");
        depend_on(&mut core, b.id, a.id);
        depend_on(&mut core, c.id, a.id);
        // `C` depends on both `A` and a subtask: it is named once, in the
        // first group.
        depend_on(&mut core, c.id, k1.id);
        depend_on(&mut core, d.id, grandchild.id);
        let mut app = app_from_core(&mut core);

        press_dd_on(&mut app, &mut core, a.id);

        assert_eq!(
            app.mode(),
            &Mode::Confirm {
                prompt: [
                    "2 task(s) depend on this task:",
                    "  B",
                    "  C",
                    "1 more depend on its subtasks (subtree delete only):",
                    "  D",
                    "\"A\" has 2 subtask(s). Delete [s]ubtree, [p]romote children, or [n] cancel?",
                ]
                .join("\n"),
                action: PendingAction::DeleteWithChildren(a.id),
            }
        );
    }

    #[test]
    fn dd_should_not_name_a_dependent_that_is_inside_the_subtree() {
        let mut core = core();
        let a = core.create_task(minimal_new_task("A")).expect("create");
        let k1 = child_of(&mut core, "K1", a.id);
        let k2 = child_of(&mut core, "K2", a.id);
        depend_on(&mut core, k2.id, k1.id);
        let mut app = app_from_core(&mut core);

        press_dd_on(&mut app, &mut core, a.id);

        assert_eq!(
            confirm_prompt(&app),
            "\"A\" has 2 subtask(s). Delete [s]ubtree, [p]romote children, or [n] cancel?"
        );
    }

    #[test]
    fn dd_should_name_a_dependent_hidden_by_the_type_filter() {
        let mut core = core();
        core.upsert_task_type(TaskType {
            key: "goal".to_string(),
            label: "Goal".to_string(),
            color: None,
            sort_order: 1,
        })
        .unwrap();
        let goal = core
            .create_task(bala_core::NewTask {
                type_key: Some("goal".to_string()),
                ..minimal_new_task("Goal")
            })
            .unwrap();
        let plain = core.create_task(minimal_new_task("Plain")).unwrap();
        depend_on(&mut core, plain.id, goal.id);
        let mut app = app_from_core(&mut core)
            .with_type_filter_state(Some("goal".to_string()), vec!["goal".to_string()]);
        super::refresh_rows_from_tree(&mut app, &mut core, Some(goal.id));
        assert_eq!(titles(&app), ["Goal"]);

        press_dd_on(&mut app, &mut core, goal.id);

        assert_eq!(
            confirm_prompt(&app),
            "1 task(s) depend on this task:\n  Plain\nDelete \"Goal\"? (y/n)"
        );
    }

    #[test]
    fn dd_should_name_a_hidden_dependent_when_clearing_the_type_filter_failed_to_refresh() {
        let remaining = std::rc::Rc::new(std::cell::Cell::new(None));
        let mut core = Core::new(FlakyStore {
            inner: InMemoryStore::default(),
            remaining: std::rc::Rc::clone(&remaining),
        })
        .expect("core should construct");
        core.upsert_task_type(TaskType {
            key: "goal".to_string(),
            label: "Goal".to_string(),
            color: None,
            sort_order: 1,
        })
        .expect("upsert type");
        let goal = core
            .create_task(bala_core::NewTask {
                type_key: Some("goal".to_string()),
                ..minimal_new_task("Goal")
            })
            .expect("create");
        let plain = core.create_task(minimal_new_task("Plain")).expect("create");
        core.add_dependency(plain.id, goal.id, bala_core::DependencyType::default())
            .expect("add dependency");
        let mut app = App::new(vec![])
            .with_type_filter_state(Some("goal".to_string()), vec!["goal".to_string()]);
        super::refresh_rows_from_tree(&mut app, &mut core, Some(goal.id));
        // Cycling the filter back to none fails to refetch, so the cached
        // tree is still the one fetched for `goal`.
        remaining.set(Some(0));
        let _ = apply_action(&mut app, &mut core, Action::CycleTypeFilter);
        assert_eq!(app.type_filter(), None);
        assert!(!app.tasks.iter().any(|task| task.id == plain.id));
        remaining.set(None);

        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);
        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);

        assert_eq!(
            app.mode(),
            &Mode::Confirm {
                prompt: "1 task(s) depend on this task:\n  Plain\nDelete \"Goal\"? (y/n)"
                    .to_string(),
                action: PendingAction::Delete(goal.id),
            }
        );
    }

    #[test]
    fn dd_should_use_the_cached_tree_when_setting_the_type_filter_failed_to_refresh() {
        let remaining = std::rc::Rc::new(std::cell::Cell::new(None));
        let mut core = Core::new(FlakyStore {
            inner: InMemoryStore::default(),
            remaining: std::rc::Rc::clone(&remaining),
        })
        .expect("core should construct");
        let a = core.create_task(minimal_new_task("A")).expect("create");
        let b = core.create_task(minimal_new_task("B")).expect("create");
        core.add_dependency(b.id, a.id, bala_core::DependencyType::default())
            .expect("add dependency");
        let mut app = App::new(vec![]).with_type_filter_state(None, vec!["goal".to_string()]);
        super::refresh_rows_from_tree(&mut app, &mut core, Some(a.id));
        // Cycling to a filter fails to refetch, so the cached tree is still
        // the unfiltered one.
        remaining.set(Some(0));
        let _ = apply_action(&mut app, &mut core, Action::CycleTypeFilter);
        assert_eq!(app.type_filter(), Some("goal"));
        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);
        // Listing the children is the only read the prompt needs.
        remaining.set(Some(1));

        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);

        assert_eq!(
            confirm_prompt(&app),
            "1 task(s) depend on this task:\n  B\nDelete \"A\"? (y/n)"
        );
    }

    #[test]
    fn dd_prompt_should_cap_each_group_and_count_the_rest() {
        let mut core = core();
        let a = core.create_task(minimal_new_task("A")).expect("create");
        let k = child_of(&mut core, "K", a.id);
        for n in 1..=7 {
            let on_a = core
                .create_task(minimal_new_task(&format!("OnA{n}")))
                .expect("create");
            depend_on(&mut core, on_a.id, a.id);
        }
        for n in 1..=6 {
            let on_k = core
                .create_task(minimal_new_task(&format!("OnK{n}")))
                .expect("create");
            depend_on(&mut core, on_k.id, k.id);
        }
        let mut app = app_from_core(&mut core);

        press_dd_on(&mut app, &mut core, a.id);

        assert_eq!(
            confirm_prompt(&app),
            [
                "7 task(s) depend on this task:",
                "  OnA1",
                "  OnA2",
                "  OnA3",
                "  OnA4",
                "  OnA5",
                "  … and 2 more",
                "6 more depend on its subtasks (subtree delete only):",
                "  OnK1",
                "  OnK2",
                "  OnK3",
                "  OnK4",
                "  OnK5",
                "  … and 1 more",
                "\"A\" has 1 subtask(s). Delete [s]ubtree, [p]romote children, or [n] cancel?",
            ]
            .join("\n")
        );
    }

    #[test]
    fn dd_should_set_error_and_stay_normal_when_the_dependents_lookup_fails() {
        let remaining = std::rc::Rc::new(std::cell::Cell::new(None));
        let mut core = Core::new(FlakyStore {
            inner: InMemoryStore::default(),
            remaining: std::rc::Rc::clone(&remaining),
        })
        .expect("core should construct");
        let p = core.create_task(minimal_new_task("P")).expect("create");
        // The unfiltered tree is only fetched while a type filter is active.
        let mut app = App::new(vec![row_for(&p)])
            .with_type_filter_state(Some("goal".to_string()), vec!["goal".to_string()]);
        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);
        // Listing the children succeeds; the tree fetch after it fails.
        remaining.set(Some(1));

        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);

        assert_eq!(app.mode(), &Mode::Normal);
        assert!(app.error().is_some());
        assert_eq!(remaining.get(), Some(0));
        remaining.set(None);
        assert!(
            core.get_tree(bala_core::TreeFilter::default())
                .expect("get_tree should succeed")
                .iter()
                .any(|t| t.id == p.id)
        );
    }

    #[test]
    fn confirm_delete_outside_a_delete_prompt_should_be_noop() {
        let mut core = core();
        let p = core.create_task(minimal_new_task("P")).expect("create");
        let _k = child_of(&mut core, "K", p.id);
        let mut app = app_from_core(&mut core);
        app.select_by_id(Some(p.id));

        let _ = apply_action(
            &mut app,
            &mut core,
            Action::ConfirmDelete(DeleteMode::Subtree),
        );

        assert_eq!(app.mode(), &Mode::Normal);
        assert!(live_tasks(&core).iter().any(|t| t.id == p.id));
    }

    /// Builds an `App` over the full tree of `core`, as `tui::run` does.
    fn app_from_core(core: &mut Core<InMemoryStore>) -> App {
        let tasks = core
            .get_tree(bala_core::TreeFilter::default())
            .expect("get_tree should succeed");
        let order = core.sibling_order().expect("sibling_order should succeed");
        let rows = crate::render::task_rows(
            &tasks,
            &order,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        );
        App::new(rows).with_tasks(tasks).with_sibling_order(order)
    }

    fn shift_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT)
    }

    fn titles(app: &App) -> Vec<String> {
        app.rows().iter().map(|r| r.title.clone()).collect()
    }

    #[test]
    fn move_down_key_should_swap_with_next_sibling_and_keep_selection_on_moved_task() {
        let mut core = core();
        let a = core.create_task(minimal_new_task("A")).expect("create");
        let _b = core.create_task(minimal_new_task("B")).expect("create");
        let mut app = app_from_core(&mut core);
        app.select_by_id(Some(a.id));

        let _ = handle_key(&mut app, &mut core, shift_key('J'));

        assert_eq!(titles(&app), vec!["B", "A"]);
        assert_eq!(app.selected_row().map(|r| r.id), Some(a.id));
        assert_eq!(app.error(), None);
    }

    #[test]
    fn move_up_key_should_be_noop_on_first_sibling() {
        let mut core = core();
        let a = core.create_task(minimal_new_task("A")).expect("create");
        let _b = core.create_task(minimal_new_task("B")).expect("create");
        let mut app = app_from_core(&mut core);
        app.select_by_id(Some(a.id));

        let _ = handle_key(&mut app, &mut core, shift_key('K'));

        assert_eq!(titles(&app), vec!["A", "B"]);
        assert_eq!(app.selected_row().map(|r| r.id), Some(a.id));
        assert_eq!(app.error(), None);
    }

    fn child_of(core: &mut Core<InMemoryStore>, title: &str, parent: TaskId) -> bala_core::Task {
        core.create_task(bala_core::NewTask {
            parent_id: Some(parent),
            ..minimal_new_task(title)
        })
        .expect("create")
    }

    #[test]
    fn indent_key_should_nest_under_previous_sibling_and_expand_it() {
        let mut core = core();
        let a = core.create_task(minimal_new_task("A")).expect("create");
        let b = core.create_task(minimal_new_task("B")).expect("create");
        let mut app = app_from_core(&mut core);
        app.collapsed.insert(a.id);
        app.select_by_id(Some(b.id));

        let _ = handle_key(&mut app, &mut core, shift_key('L'));

        assert_eq!(titles(&app), vec!["A", "B"]);
        assert!(!app.collapsed.contains(&a.id));
        let sel = app.selected_row().expect("selection");
        assert_eq!((sel.id, sel.parent_id, sel.depth), (b.id, Some(a.id), 1));
        assert_eq!(app.error(), None);
    }

    #[test]
    fn indent_key_should_expand_the_parent_core_chose_after_a_sibling_was_deleted() {
        let mut core = core();
        let a = core.create_task(minimal_new_task("A")).expect("create");
        let b = core.create_task(minimal_new_task("B")).expect("create");
        let c = core.create_task(minimal_new_task("C")).expect("create");
        let mut app = app_from_core(&mut core);
        app.collapsed.insert(a.id);
        app.select_by_id(Some(b.id));
        let _ = handle_key(
            &mut app,
            &mut core,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
        );
        let _ = handle_key(
            &mut app,
            &mut core,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
        );
        let _ = handle_key(
            &mut app,
            &mut core,
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
        );
        app.select_by_id(Some(c.id));

        let _ = handle_key(&mut app, &mut core, shift_key('L'));

        // Core nests C under A (B is gone); A must be expanded and C selected.
        assert!(!app.collapsed.contains(&a.id));
        let sel = app.selected_row().expect("selection");
        assert_eq!((sel.id, sel.parent_id), (c.id, Some(a.id)));
    }

    /// Builds a `goal` with two `task` children, A then B, and an `App`
    /// filtered to `task` rows: the goal is hidden, so A and B render as
    /// roots though their parent is still the goal. Returns
    /// `(app, goal, a, b)`.
    fn app_with_parent_hidden_by_the_filter(
        core: &mut Core<InMemoryStore>,
    ) -> (App, TaskId, TaskId, TaskId) {
        core.upsert_task_type(TaskType {
            key: "goal".to_string(),
            label: "Goal".to_string(),
            color: None,
            sort_order: 1,
        })
        .expect("upsert_task_type should succeed");
        let goal = core
            .create_task(bala_core::NewTask {
                type_key: Some("goal".to_string()),
                ..minimal_new_task("Goal")
            })
            .expect("create");
        let a = child_of(core, "A", goal.id);
        let b = child_of(core, "B", goal.id);
        let mut app = App::new(vec![])
            .with_type_filter_state(Some("task".to_string()), vec!["task".to_string()]);
        super::refresh_rows_from_tree(&mut app, core, None);
        (app, goal.id, a.id, b.id)
    }

    #[test]
    fn outdent_key_should_be_a_no_op_when_the_parent_is_hidden_by_the_filter() {
        let mut core = core();
        let (mut app, goal, a, b) = app_with_parent_hidden_by_the_filter(&mut core);
        app.select_by_id(Some(b));
        let rendered = app.selected_row().expect("selection");
        assert_eq!((rendered.parent_id, rendered.depth), (None, 0));
        let before = core.get_task(b).expect("get_task").expect("task exists");

        let _ = handle_key(&mut app, &mut core, shift_key('H'));

        let after = core.get_task(b).expect("get_task").expect("task exists");
        assert_eq!(after.parent_id, Some(goal));
        assert_eq!(after, before);
        let order = core.sibling_order().expect("sibling_order");
        assert_eq!(order.children_of(Some(goal)), [a, b]);
        assert_eq!(order.children_of(None), [goal]);
        assert_eq!(titles(&app), vec!["A", "B"]);
        assert_eq!(app.selected_row().map(|r| r.id), Some(b));
        assert_eq!(app.error(), None);
    }

    #[test]
    fn indent_key_should_be_a_no_op_when_the_parent_is_hidden_by_the_filter() {
        let mut core = core();
        let (mut app, goal, a, b) = app_with_parent_hidden_by_the_filter(&mut core);
        app.collapsed.insert(a);
        app.select_by_id(Some(b));
        let before = core.get_task(b).expect("get_task").expect("task exists");

        let _ = handle_key(&mut app, &mut core, shift_key('L'));

        let after = core.get_task(b).expect("get_task").expect("task exists");
        assert_eq!(after.parent_id, Some(goal));
        assert_eq!(after, before);
        let order = core.sibling_order().expect("sibling_order");
        assert_eq!(order.children_of(Some(goal)), [a, b]);
        assert_eq!(order.children_of(Some(a)), []);
        assert!(app.collapsed.contains(&a));
        assert_eq!(titles(&app), vec!["A", "B"]);
        let sel = app.selected_row().expect("selection");
        assert_eq!((sel.id, sel.parent_id, sel.depth), (b, None, 0));
        assert_eq!(app.error(), None);
    }

    #[test]
    fn move_indent_and_outdent_keys_should_show_an_inline_error_when_core_rejects_them() {
        for key in ['J', 'K', 'L', 'H'] {
            let mut core = core();
            let a = core.create_task(minimal_new_task("A")).expect("create");
            let mut app = app_from_core(&mut core);
            app.select_by_id(Some(a.id));
            // The task is deleted behind the list's back, so its row is stale.
            core.delete_task(a.id, DeleteMode::Subtree).expect("delete");

            let _ = handle_key(&mut app, &mut core, shift_key(key));

            assert!(app.error().is_some(), "key {key} should surface NotFound");
            assert_eq!(titles(&app), vec!["A"]);
        }
    }

    #[test]
    fn indent_key_should_be_noop_on_first_sibling() {
        let mut core = core();
        let a = core.create_task(minimal_new_task("A")).expect("create");
        let _b = core.create_task(minimal_new_task("B")).expect("create");
        let mut app = app_from_core(&mut core);
        app.select_by_id(Some(a.id));

        let _ = handle_key(&mut app, &mut core, shift_key('L'));

        assert_eq!(titles(&app), vec!["A", "B"]);
        assert_eq!(app.selected_row().map(|r| (r.id, r.depth)), Some((a.id, 0)));
        assert_eq!(app.error(), None);
    }

    #[test]
    fn outdent_key_should_move_after_old_parent() {
        let mut core = core();
        let p = core.create_task(minimal_new_task("P")).expect("create");
        let x = child_of(&mut core, "X", p.id);
        let _q = core.create_task(minimal_new_task("Q")).expect("create");
        let mut app = app_from_core(&mut core);
        app.select_by_id(Some(x.id));

        let _ = handle_key(&mut app, &mut core, shift_key('H'));

        assert_eq!(titles(&app), vec!["P", "X", "Q"]);
        let sel = app.selected_row().expect("selection");
        assert_eq!((sel.id, sel.parent_id, sel.depth), (x.id, None, 0));
        assert_eq!(app.error(), None);
    }

    #[test]
    fn outdent_key_should_move_a_nested_task_to_its_grandparent() {
        let mut core = core();
        let g = core.create_task(minimal_new_task("G")).expect("create");
        let p = child_of(&mut core, "P", g.id);
        let x = child_of(&mut core, "X", p.id);
        let _q = child_of(&mut core, "Q", g.id);
        let mut app = app_from_core(&mut core);
        app.select_by_id(Some(x.id));

        let _ = handle_key(&mut app, &mut core, shift_key('H'));

        assert_eq!(titles(&app), vec!["G", "P", "X", "Q"]);
        let sel = app.selected_row().expect("selection");
        assert_eq!((sel.id, sel.parent_id, sel.depth), (x.id, Some(g.id), 1));
        assert_eq!(app.error(), None);
    }

    #[test]
    fn outdent_key_should_be_noop_at_top_level() {
        let mut core = core();
        let a = core.create_task(minimal_new_task("A")).expect("create");
        let mut app = app_from_core(&mut core);
        app.select_by_id(Some(a.id));

        let _ = handle_key(&mut app, &mut core, shift_key('H'));

        assert_eq!(titles(&app), vec!["A"]);
        assert_eq!(app.selected_row().map(|r| r.id), Some(a.id));
        assert_eq!(app.error(), None);
    }

    fn row_for(task: &bala_core::Task) -> TaskRow {
        TaskRow {
            id: task.id,
            title: task.title.clone(),
            type_label: "task".to_string(),
            status: task.status,
            assignee_name: None,
            depth: 0,
            has_children: false,
            collapsed: false,
            direct_summary: None,
            parent_id: None,
            blocked: false,
        }
    }

    fn minimal_new_task(title: &str) -> bala_core::NewTask {
        bala_core::NewTask {
            title: title.to_string(),
            description: None,
            parent_id: None,
            type_key: None,
            start_date: None,
            due_date: None,
            duration_days: None,
            assignee_id: None,
        }
    }

    #[test]
    fn apply_action_toggle_complete_should_complete_incomplete_leaf_and_update_row_status() {
        let mut core = core();
        let task = core
            .create_task(minimal_new_task("Write docs"))
            .expect("create_task should succeed");
        let mut app = App::new(vec![row_for(&task)]);

        let _ = apply_action(&mut app, &mut core, Action::ToggleComplete);

        assert_eq!(app.mode(), &Mode::Normal);
        assert_eq!(app.rows()[0].status, TaskStatus::Complete);
        assert_eq!(app.error(), None);
    }

    #[test]
    fn apply_action_toggle_complete_should_reopen_completed_task_and_update_row_status() {
        let mut core = core();
        let task = core
            .create_task(minimal_new_task("Write docs"))
            .expect("create_task should succeed");
        core.complete_task(task.id, false)
            .expect("complete_task should succeed");
        let mut row = row_for(&task);
        row.status = TaskStatus::Complete;
        let mut app = App::new(vec![row]);

        let _ = apply_action(&mut app, &mut core, Action::ToggleComplete);

        assert_eq!(app.mode(), &Mode::Normal);
        assert_eq!(app.rows()[0].status, TaskStatus::Incomplete);
        assert_eq!(app.error(), None);
    }

    #[test]
    fn apply_action_toggle_complete_with_incomplete_children_should_enter_confirm_mode_with_cascade_prompt()
     {
        let mut core = core();
        let parent = core
            .create_task(minimal_new_task("Parent"))
            .expect("create_task should succeed");
        let _child = core
            .create_task(bala_core::NewTask {
                parent_id: Some(parent.id),
                ..minimal_new_task("Child")
            })
            .expect("create_task should succeed");
        let mut app = App::new(vec![row_for(&parent)]);

        let _ = apply_action(&mut app, &mut core, Action::ToggleComplete);

        match app.mode() {
            Mode::Confirm { prompt, action } => {
                assert!(prompt.contains("Parent"));
                assert!(prompt.contains('1'));
                assert_eq!(
                    *action,
                    crate::tui::mode::PendingAction::CompleteCascade(parent.id)
                );
            }
            other => panic!("expected Mode::Confirm, got {other:?}"),
        }
    }

    #[test]
    fn apply_action_toggle_complete_with_multi_level_incomplete_descendants_should_count_full_cascade()
     {
        let mut core = core();
        let parent = core
            .create_task(minimal_new_task("Parent"))
            .expect("create_task should succeed");
        let child = core
            .create_task(bala_core::NewTask {
                parent_id: Some(parent.id),
                ..minimal_new_task("Child")
            })
            .expect("create_task should succeed");
        let _grandchild = core
            .create_task(bala_core::NewTask {
                parent_id: Some(child.id),
                ..minimal_new_task("Grandchild")
            })
            .expect("create_task should succeed");
        let mut app = App::new(vec![row_for(&parent)]);

        let _ = apply_action(&mut app, &mut core, Action::ToggleComplete);

        match app.mode() {
            Mode::Confirm { prompt, .. } => {
                // Two incomplete descendants (child + grandchild) will be
                // completed by the cascade, not just the one direct child —
                // the prompt must name the full cascade count, not just
                // direct children.
                assert!(
                    prompt.contains('2'),
                    "prompt should report both incomplete descendants, got: {prompt:?}"
                );
            }
            other => panic!("expected Mode::Confirm, got {other:?}"),
        }
    }

    #[test]
    fn apply_action_confirm_yes_complete_cascade_should_call_complete_task_with_cascade_true_and_update_rows()
     {
        let mut core = core();
        let parent = core
            .create_task(minimal_new_task("Parent"))
            .expect("create_task should succeed");
        let child = core
            .create_task(bala_core::NewTask {
                parent_id: Some(parent.id),
                ..minimal_new_task("Child")
            })
            .expect("create_task should succeed");
        let mut app = App::new(vec![row_for(&parent), row_for(&child)]);
        let _ = apply_action(&mut app, &mut core, Action::ToggleComplete);

        let _ = apply_action(&mut app, &mut core, Action::ConfirmYes);

        assert_eq!(app.mode(), &Mode::Normal);
        assert_eq!(app.error(), None);
        let parent_row = app.rows().iter().find(|row| row.id == parent.id).unwrap();
        let child_row = app.rows().iter().find(|row| row.id == child.id).unwrap();
        assert_eq!(parent_row.status, TaskStatus::Complete);
        assert_eq!(child_row.status, TaskStatus::Complete);
    }

    #[test]
    fn apply_action_confirm_no_should_leave_task_incomplete_after_cascade_prompt() {
        let mut core = core();
        let parent = core
            .create_task(minimal_new_task("Parent"))
            .expect("create_task should succeed");
        let _child = core
            .create_task(bala_core::NewTask {
                parent_id: Some(parent.id),
                ..minimal_new_task("Child")
            })
            .expect("create_task should succeed");
        let mut app = App::new(vec![row_for(&parent)]);
        let _ = apply_action(&mut app, &mut core, Action::ToggleComplete);

        let _ = apply_action(&mut app, &mut core, Action::ConfirmNo);

        assert_eq!(app.mode(), &Mode::Normal);
        assert_eq!(app.rows()[0].status, TaskStatus::Incomplete);
        let tasks = core
            .get_tree(bala_core::TreeFilter::default())
            .expect("get_tree should succeed");
        let updated = tasks
            .into_iter()
            .find(|task| task.id == parent.id)
            .expect("task should still exist");
        assert_eq!(updated.status, TaskStatus::Incomplete);
    }

    #[test]
    fn apply_action_toggle_complete_with_no_selection_should_be_noop() {
        let mut app = App::new(vec![]);
        let mut core = core();

        let _ = apply_action(&mut app, &mut core, Action::ToggleComplete);

        assert_eq!(app.mode(), &Mode::Normal);
        assert_eq!(app.rows(), []);
        assert_eq!(app.error(), None);
    }

    #[test]
    fn select_by_id_should_select_matching_row() {
        let row_a = row("First");
        let row_b = row("Second");
        let id_b = row_b.id;
        let mut app = App::new(vec![row_a, row_b.clone()]);

        app.select_by_id(Some(id_b));

        assert_eq!(app.selected_row(), Some(&row_b));
    }

    #[test]
    fn select_by_id_should_leave_default_selection_when_id_not_found() {
        let row_a = row("First");
        let row_b = row("Second");
        let mut app = App::new(vec![row_a.clone(), row_b]);

        app.select_by_id(Some(TaskId::new()));

        assert_eq!(app.selected_row(), Some(&row_a));
    }

    #[test]
    fn apply_action_start_insert_new_subtask_should_enter_insert_mode_scoped_to_selected_task() {
        let task_row = row("Parent");
        let id = task_row.id;
        let mut app = App::new(vec![task_row]);
        let mut core = core();

        let _ = apply_action(&mut app, &mut core, Action::StartInsertNewSubtask);

        assert_eq!(
            app.mode(),
            &Mode::Insert {
                field: EditableField::NewSubtaskTitle(id),
                buffer: String::new(),
            }
        );
    }

    #[test]
    fn apply_action_start_insert_new_subtask_with_no_selection_should_be_noop() {
        let mut app = App::new(vec![]);
        let mut core = core();

        let _ = apply_action(&mut app, &mut core, Action::StartInsertNewSubtask);

        assert_eq!(app.mode(), &Mode::Normal);
    }

    #[test]
    fn apply_action_submit_new_subtask_with_no_inheritable_parent_fields_should_return_to_normal_directly()
     {
        let mut core = core();
        let parent = core
            .create_task(minimal_new_task("Parent"))
            .expect("create_task should succeed");
        let mut app = App::new(vec![row_for(&parent)]);
        let _ = apply_action(&mut app, &mut core, Action::StartInsertNewSubtask);
        for c in "Subtask".chars() {
            let _ = apply_action(&mut app, &mut core, Action::InsertChar(c));
        }

        let _ = apply_action(&mut app, &mut core, Action::SubmitInsert);

        assert_eq!(app.mode(), &Mode::Normal);
        let tasks = core
            .get_tree(bala_core::TreeFilter::default())
            .expect("get_tree should succeed");
        let child = tasks
            .iter()
            .find(|task| task.title == "Subtask")
            .expect("subtask should have been created");
        assert_eq!(child.parent_id, Some(parent.id));
    }

    #[test]
    fn apply_action_submit_new_subtask_with_inheritable_parent_fields_should_enter_confirm_mode() {
        let mut core = core();
        let user = core
            .create_user("Alice".to_string())
            .expect("add_user should succeed");
        let parent = core
            .create_task(bala_core::NewTask {
                assignee_id: Some(user.id),
                ..minimal_new_task("Parent")
            })
            .expect("create_task should succeed");
        let mut app = App::new(vec![row_for(&parent)]);
        let _ = apply_action(&mut app, &mut core, Action::StartInsertNewSubtask);
        for c in "Subtask".chars() {
            let _ = apply_action(&mut app, &mut core, Action::InsertChar(c));
        }

        let _ = apply_action(&mut app, &mut core, Action::SubmitInsert);

        let tasks = core
            .get_tree(bala_core::TreeFilter::default())
            .expect("get_tree should succeed");
        let child = tasks
            .iter()
            .find(|task| task.title == "Subtask")
            .expect("subtask should have been created");

        match app.mode() {
            Mode::Confirm { action, .. } => {
                assert_eq!(
                    *action,
                    crate::tui::mode::PendingAction::InheritFromParent {
                        child: child.id,
                        parent: parent.id,
                    }
                );
            }
            other => panic!("expected Mode::Confirm, got {other:?}"),
        }
    }

    #[test]
    fn apply_action_confirm_yes_inherit_should_copy_parent_assignee_and_dates_onto_child() {
        let mut core = core();
        let user = core
            .create_user("Alice".to_string())
            .expect("add_user should succeed");
        let start = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let due = chrono::NaiveDate::from_ymd_opt(2026, 1, 31).unwrap();
        let parent = core
            .create_task(bala_core::NewTask {
                assignee_id: Some(user.id),
                start_date: Some(start),
                due_date: Some(due),
                ..minimal_new_task("Parent")
            })
            .expect("create_task should succeed");
        let mut app = App::new(vec![row_for(&parent)]);
        let _ = apply_action(&mut app, &mut core, Action::StartInsertNewSubtask);
        for c in "Subtask".chars() {
            let _ = apply_action(&mut app, &mut core, Action::InsertChar(c));
        }
        let _ = apply_action(&mut app, &mut core, Action::SubmitInsert);
        let child_id = match app.mode() {
            Mode::Confirm {
                action: crate::tui::mode::PendingAction::InheritFromParent { child, .. },
                ..
            } => *child,
            other => panic!("expected Mode::Confirm, got {other:?}"),
        };

        let _ = apply_action(&mut app, &mut core, Action::ConfirmYes);

        assert_eq!(app.mode(), &Mode::Normal);
        let child = core
            .get_task(child_id)
            .expect("get_task should succeed")
            .expect("child should exist");
        assert_eq!(child.assignee_id, Some(user.id));
        assert_eq!(child.start_date, Some(start));
        assert_eq!(child.due_date, Some(due));
        // Dates copied from the parent are entered dates like any other.
        assert!(child.dates_fixed);
    }

    #[test]
    fn apply_action_confirm_no_inherit_should_leave_child_fields_unset() {
        let mut core = core();
        let user = core
            .create_user("Alice".to_string())
            .expect("add_user should succeed");
        let parent = core
            .create_task(bala_core::NewTask {
                assignee_id: Some(user.id),
                ..minimal_new_task("Parent")
            })
            .expect("create_task should succeed");
        let mut app = App::new(vec![row_for(&parent)]);
        let _ = apply_action(&mut app, &mut core, Action::StartInsertNewSubtask);
        for c in "Subtask".chars() {
            let _ = apply_action(&mut app, &mut core, Action::InsertChar(c));
        }
        let _ = apply_action(&mut app, &mut core, Action::SubmitInsert);
        let child_id = match app.mode() {
            Mode::Confirm {
                action: crate::tui::mode::PendingAction::InheritFromParent { child, .. },
                ..
            } => *child,
            other => panic!("expected Mode::Confirm, got {other:?}"),
        };

        let _ = apply_action(&mut app, &mut core, Action::ConfirmNo);

        assert_eq!(app.mode(), &Mode::Normal);
        let child = core
            .get_task(child_id)
            .expect("get_task should succeed")
            .expect("child should exist");
        assert_eq!(child.assignee_id, None);
        assert_eq!(child.start_date, None);
        assert_eq!(child.due_date, None);
    }

    #[test]
    fn apply_action_submit_new_subtask_should_nest_new_row_under_parent() {
        let mut core = core();
        let parent = core
            .create_task(minimal_new_task("Parent"))
            .expect("create_task should succeed");
        let mut app = App::new(vec![row_for(&parent)]);

        let _ = apply_action(&mut app, &mut core, Action::StartInsertNewSubtask);
        for c in "Subtask".chars() {
            let _ = apply_action(&mut app, &mut core, Action::InsertChar(c));
        }
        let _ = apply_action(&mut app, &mut core, Action::SubmitInsert);

        let parent_row = app
            .rows()
            .iter()
            .find(|row| row.id == parent.id)
            .expect("parent row should still be present");
        let parent_depth = parent_row.depth;
        let child_row = app
            .rows()
            .iter()
            .find(|row| row.title == "Subtask")
            .expect("child row should be present");
        assert_eq!(child_row.depth, parent_depth + 1);
    }

    #[test]
    fn start_reparent_should_prefill_the_current_parent_id() {
        let mut core = core();
        let parent = core
            .create_task(minimal_new_task("Parent"))
            .expect("create_task should succeed");
        let child = core
            .create_task(bala_core::NewTask {
                parent_id: Some(parent.id),
                ..minimal_new_task("Child")
            })
            .expect("create_task should succeed");
        let mut app = App::new(vec![row_for(&child)]);

        let _ = apply_action(&mut app, &mut core, Action::StartReparent);

        match app.mode() {
            Mode::Insert {
                field: EditableField::Parent(id),
                buffer,
            } => {
                assert_eq!(*id, child.id);
                assert_eq!(*buffer, uuid::Uuid::from(parent.id).to_string());
            }
            other => panic!("expected Mode::Insert with Parent field, got {other:?}"),
        }
    }

    #[test]
    fn start_reparent_should_prefill_an_empty_buffer_for_a_top_level_task() {
        let mut core = core();
        let task = core
            .create_task(minimal_new_task("Top level"))
            .expect("create_task should succeed");
        let mut app = App::new(vec![row_for(&task)]);

        let _ = apply_action(&mut app, &mut core, Action::StartReparent);

        assert_eq!(
            app.mode(),
            &Mode::Insert {
                field: EditableField::Parent(task.id),
                buffer: String::new(),
            }
        );
    }

    #[test]
    fn submit_reparent_should_reject_more_than_one_id_and_stay_in_insert() {
        let mut core = core();
        let task_a = core
            .create_task(minimal_new_task("A"))
            .expect("create_task should succeed");
        let task_b = core
            .create_task(minimal_new_task("B"))
            .expect("create_task should succeed");
        let task_c = core
            .create_task(minimal_new_task("C"))
            .expect("create_task should succeed");
        let mut app = App::new(vec![row_for(&task_a), row_for(&task_b), row_for(&task_c)]);
        let _ = apply_action(&mut app, &mut core, Action::StartReparent);
        let typed = format!(
            "{},{}",
            uuid::Uuid::from(task_b.id),
            uuid::Uuid::from(task_c.id)
        );
        for c in typed.chars() {
            let _ = apply_action(&mut app, &mut core, Action::InsertChar(c));
        }

        let _ = apply_action(&mut app, &mut core, Action::SubmitInsert);

        assert_eq!(
            app.mode(),
            &Mode::Insert {
                field: EditableField::Parent(task_a.id),
                buffer: typed,
            }
        );
        assert_eq!(app.error(), Some("a task can have only one parent"));
        let unchanged = core
            .get_task(task_a.id)
            .expect("get_task should succeed")
            .expect("task A should exist");
        assert_eq!(unchanged.parent_id, None);
    }

    #[test]
    fn apply_action_submit_reparent_should_call_set_parent_and_move_task_in_tree() {
        let mut core = core();
        let task_a = core
            .create_task(minimal_new_task("A"))
            .expect("create_task should succeed");
        let task_b = core
            .create_task(minimal_new_task("B"))
            .expect("create_task should succeed");
        let mut app = App::new(vec![row_for(&task_a), row_for(&task_b)]);
        let _ = apply_action(&mut app, &mut core, Action::StartReparent);
        for c in uuid::Uuid::from(task_b.id).to_string().chars() {
            let _ = apply_action(&mut app, &mut core, Action::InsertChar(c));
        }

        let _ = apply_action(&mut app, &mut core, Action::SubmitInsert);

        assert_eq!(app.mode(), &Mode::Normal);
        assert_eq!(app.error(), None);
        let b_row = app
            .rows()
            .iter()
            .find(|row| row.id == task_b.id)
            .expect("B row should still be present");
        let a_row = app
            .rows()
            .iter()
            .find(|row| row.id == task_a.id)
            .expect("A row should still be present");
        assert_eq!(a_row.depth, b_row.depth + 1);
    }

    #[test]
    fn submit_reparent_with_empty_line_should_promote_to_top_level() {
        let mut core = core();
        let parent = core
            .create_task(minimal_new_task("Parent"))
            .expect("create_task should succeed");
        let child = core
            .create_task(bala_core::NewTask {
                parent_id: Some(parent.id),
                ..minimal_new_task("Child")
            })
            .expect("create_task should succeed");
        let mut app = App::new(vec![row_for(&parent), row_for(&child)]);
        let _ = apply_action(&mut app, &mut core, Action::StartReparent);
        // Selection is still on `parent` (row 0); focus the child instead by
        // re-entering StartReparent after moving down, so the prefilled
        // buffer (parent's own — empty, since it's top-level) isn't the one
        // under test. Instead, directly move to the child row.
        let _ = apply_action(&mut app, &mut core, Action::CancelInsert);
        app.move_down();
        let _ = apply_action(&mut app, &mut core, Action::StartReparent);
        let buffer_len = match app.mode() {
            Mode::Insert {
                field: EditableField::Parent(id),
                buffer,
            } => {
                assert_eq!(*id, child.id);
                assert_ne!(buffer, "");
                buffer.chars().count()
            }
            other => panic!("expected Mode::Insert with Parent field, got {other:?}"),
        };
        for _ in 0..buffer_len {
            let _ = apply_action(&mut app, &mut core, Action::Backspace);
        }

        let _ = apply_action(&mut app, &mut core, Action::SubmitInsert);

        assert_eq!(app.mode(), &Mode::Normal);
        assert_eq!(app.error(), None);
        let updated_child = core
            .get_task(child.id)
            .expect("get_task should succeed")
            .expect("child should exist");
        assert_eq!(updated_child.parent_id, None);
        let child_row = app
            .rows()
            .iter()
            .find(|row| row.id == child.id)
            .expect("child row should still be present");
        assert_eq!(child_row.depth, 0);
    }

    #[test]
    fn apply_action_submit_reparent_with_circular_parent_should_show_inline_error_and_stay_in_insert_mode()
     {
        let mut core = core();
        let task_a = core
            .create_task(minimal_new_task("A"))
            .expect("create_task should succeed");
        let mut app = App::new(vec![row_for(&task_a)]);
        let _ = apply_action(&mut app, &mut core, Action::StartReparent);
        for c in uuid::Uuid::from(task_a.id).to_string().chars() {
            let _ = apply_action(&mut app, &mut core, Action::InsertChar(c));
        }

        let _ = apply_action(&mut app, &mut core, Action::SubmitInsert);

        assert!(matches!(
            app.mode(),
            Mode::Insert {
                field: EditableField::Parent(id),
                ..
            } if *id == task_a.id
        ));
        assert!(app.error().is_some());
    }

    #[test]
    fn apply_action_submit_reparent_with_invalid_uuid_should_show_inline_error() {
        let mut core = core();
        let task_a = core
            .create_task(minimal_new_task("A"))
            .expect("create_task should succeed");
        let mut app = App::new(vec![row_for(&task_a)]);
        let _ = apply_action(&mut app, &mut core, Action::StartReparent);
        for c in "not-a-uuid".chars() {
            let _ = apply_action(&mut app, &mut core, Action::InsertChar(c));
        }

        let _ = apply_action(&mut app, &mut core, Action::SubmitInsert);

        assert!(matches!(
            app.mode(),
            Mode::Insert {
                field: EditableField::Parent(id),
                ..
            } if *id == task_a.id
        ));
        assert!(app.error().is_some());
    }

    #[test]
    fn apply_action_start_set_type_should_prefill_buffer_with_current_type_key() {
        let mut core = core();
        core.upsert_task_type(TaskType {
            key: "goal".to_owned(),
            label: "Goal".to_owned(),
            color: None,
            sort_order: 1,
        })
        .expect("upsert_task_type should succeed");
        let task = core
            .create_task(bala_core::NewTask {
                type_key: Some("goal".to_owned()),
                ..minimal_new_task("Task")
            })
            .expect("create_task should succeed");
        let mut app = App::new(vec![row_for(&task)]);

        let _ = apply_action(&mut app, &mut core, Action::StartSetType);

        match app.mode() {
            Mode::Insert {
                field: EditableField::TypeKey(id),
                buffer,
            } => {
                assert_eq!(*id, task.id);
                assert_eq!(buffer, "goal");
            }
            other => panic!("expected Mode::Insert with TypeKey field, got {other:?}"),
        }
    }

    #[test]
    fn apply_action_submit_set_type_should_update_task_type_key() {
        let mut core = core();
        core.upsert_task_type(TaskType {
            key: "goal".to_owned(),
            label: "Goal".to_owned(),
            color: None,
            sort_order: 1,
        })
        .expect("upsert_task_type should succeed");
        let task = core
            .create_task(minimal_new_task("Task"))
            .expect("create_task should succeed");
        let mut app = App::new(vec![row_for(&task)]);
        app.mode = Mode::Insert {
            field: EditableField::TypeKey(task.id),
            buffer: "goal".to_string(),
        };

        let _ = apply_action(&mut app, &mut core, Action::SubmitInsert);

        assert_eq!(app.mode(), &Mode::Normal);
        assert_eq!(app.error(), None);
        let updated = core
            .get_task(task.id)
            .expect("get_task should succeed")
            .expect("task should exist");
        assert_eq!(updated.type_key, "goal");
    }

    #[test]
    fn apply_action_submit_set_type_with_unknown_type_should_show_inline_error_and_stay_in_insert_mode()
     {
        let mut core = core();
        let task = core
            .create_task(minimal_new_task("Task"))
            .expect("create_task should succeed");
        let mut app = App::new(vec![row_for(&task)]);
        app.mode = Mode::Insert {
            field: EditableField::TypeKey(task.id),
            buffer: "bogus-type".to_string(),
        };

        let _ = apply_action(&mut app, &mut core, Action::SubmitInsert);

        assert_eq!(
            app.mode(),
            &Mode::Insert {
                field: EditableField::TypeKey(task.id),
                buffer: "bogus-type".to_string(),
            }
        );
        assert!(app.error().is_some());
    }

    #[test]
    fn select_by_id_with_none_should_be_noop() {
        let row_a = row("First");
        let row_b = row("Second");
        let mut app = App::new(vec![row_a.clone(), row_b]);

        app.select_by_id(None);

        assert_eq!(app.selected_row(), Some(&row_a));
    }

    #[test]
    fn collapse_focused_should_hide_children_and_keep_selection_on_parent() {
        let mut core = core();
        let parent = core
            .create_task(minimal_new_task("Parent"))
            .expect("create_task should succeed");
        let child = core
            .create_task(bala_core::NewTask {
                parent_id: Some(parent.id),
                ..minimal_new_task("Child")
            })
            .expect("create_task should succeed");
        let tasks = vec![parent.clone(), child.clone()];
        let rows = crate::render::task_rows(
            &tasks,
            &crate::render::order_of(&tasks),
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        );
        let mut app = App::new(rows).with_tasks(tasks);

        let _ = apply_action(&mut app, &mut core, Action::CollapseFocused);

        assert_eq!(app.rows().len(), 1);
        assert_eq!(app.selected_row().map(|row| row.id), Some(parent.id));
        assert!(app.rows()[0].collapsed);
    }

    #[test]
    fn collapse_focused_on_leaf_task_should_be_noop() {
        let mut core = core();
        let leaf = core
            .create_task(minimal_new_task("Leaf"))
            .expect("create_task should succeed");
        let tasks = vec![leaf.clone()];
        let rows = crate::render::task_rows(
            &tasks,
            &crate::render::order_of(&tasks),
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        );
        let mut app = App::new(rows).with_tasks(tasks);

        let _ = apply_action(&mut app, &mut core, Action::CollapseFocused);

        assert_eq!(app.rows().len(), 1);
        assert!(!app.rows()[0].collapsed);
    }

    #[test]
    fn expand_focused_should_reveal_previously_hidden_children() {
        let mut core = core();
        let parent = core
            .create_task(minimal_new_task("Parent"))
            .expect("create_task should succeed");
        let child = core
            .create_task(bala_core::NewTask {
                parent_id: Some(parent.id),
                ..minimal_new_task("Child")
            })
            .expect("create_task should succeed");
        let tasks = vec![parent.clone(), child.clone()];
        let rows = crate::render::task_rows(
            &tasks,
            &crate::render::order_of(&tasks),
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        );
        let mut app = App::new(rows).with_tasks(tasks);
        let _ = apply_action(&mut app, &mut core, Action::CollapseFocused);

        let _ = apply_action(&mut app, &mut core, Action::ExpandFocused);

        assert_eq!(app.rows().len(), 2);
        assert!(!app.rows()[0].collapsed);
    }

    #[test]
    fn expand_all_should_clear_all_collapsed_state() {
        let mut core = core();
        let parent_a = core
            .create_task(minimal_new_task("Parent A"))
            .expect("create_task should succeed");
        let _child_a = core
            .create_task(bala_core::NewTask {
                parent_id: Some(parent_a.id),
                ..minimal_new_task("Child A")
            })
            .expect("create_task should succeed");
        let parent_b = core
            .create_task(minimal_new_task("Parent B"))
            .expect("create_task should succeed");
        let _child_b = core
            .create_task(bala_core::NewTask {
                parent_id: Some(parent_b.id),
                ..minimal_new_task("Child B")
            })
            .expect("create_task should succeed");
        let tasks = core
            .get_tree(bala_core::TreeFilter::default())
            .expect("get_tree should succeed");
        let rows = crate::render::task_rows(
            &tasks,
            &crate::render::order_of(&tasks),
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        );
        let mut app = App::new(rows).with_tasks(tasks.clone());
        let _ = apply_action(&mut app, &mut core, Action::CollapseAll);

        let _ = apply_action(&mut app, &mut core, Action::ExpandAll);

        assert_eq!(app.rows().len(), tasks.len());
        for row in app.rows() {
            if row.has_children {
                assert!(!row.collapsed);
            }
        }
    }

    #[test]
    #[allow(clippy::similar_names)] // parent_a_row/parent_b_row read clearly paired with parent_a/parent_b above
    fn collapse_all_should_hide_every_subtree() {
        let mut core = core();
        let parent_a = core
            .create_task(minimal_new_task("Parent A"))
            .expect("create_task should succeed");
        let _child_a = core
            .create_task(bala_core::NewTask {
                parent_id: Some(parent_a.id),
                ..minimal_new_task("Child A")
            })
            .expect("create_task should succeed");
        let parent_b = core
            .create_task(minimal_new_task("Parent B"))
            .expect("create_task should succeed");
        let _child_b = core
            .create_task(bala_core::NewTask {
                parent_id: Some(parent_b.id),
                ..minimal_new_task("Child B")
            })
            .expect("create_task should succeed");
        let leaf = core
            .create_task(minimal_new_task("Leaf"))
            .expect("create_task should succeed");
        let tasks = core
            .get_tree(bala_core::TreeFilter::default())
            .expect("get_tree should succeed");
        let rows = crate::render::task_rows(
            &tasks,
            &crate::render::order_of(&tasks),
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        );
        let mut app = App::new(rows).with_tasks(tasks);

        let _ = apply_action(&mut app, &mut core, Action::CollapseAll);

        assert_eq!(app.rows().len(), 3);
        let parent_a_row = app
            .rows()
            .iter()
            .find(|row| row.id == parent_a.id)
            .expect("parent A row should still be present");
        assert!(parent_a_row.collapsed);
        let parent_b_row = app
            .rows()
            .iter()
            .find(|row| row.id == parent_b.id)
            .expect("parent B row should still be present");
        assert!(parent_b_row.collapsed);
        let leaf_row = app
            .rows()
            .iter()
            .find(|row| row.id == leaf.id)
            .expect("leaf row should still be present");
        assert!(!leaf_row.collapsed);
    }

    #[test]
    fn collapse_focused_should_be_idempotent_when_already_collapsed() {
        let mut core = core();
        let parent = core
            .create_task(minimal_new_task("Parent"))
            .expect("create_task should succeed");
        let child = core
            .create_task(bala_core::NewTask {
                parent_id: Some(parent.id),
                ..minimal_new_task("Child")
            })
            .expect("create_task should succeed");
        let tasks = vec![parent.clone(), child.clone()];
        let rows = crate::render::task_rows(
            &tasks,
            &crate::render::order_of(&tasks),
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        );
        let mut app = App::new(rows).with_tasks(tasks);
        let _ = apply_action(&mut app, &mut core, Action::CollapseFocused);
        let rows_len_after_first_collapse = app.rows().len();

        let _ = apply_action(&mut app, &mut core, Action::CollapseFocused);

        assert_eq!(app.rows().len(), rows_len_after_first_collapse);
        assert_eq!(app.selected_row().map(|row| row.id), Some(parent.id));
    }

    #[test]
    fn rebuild_rows_after_toggle_complete_should_not_revert_status_or_summary() {
        // Regression: `refresh_row_statuses` used to patch only `app.rows`,
        // leaving `app.tasks` (the cache `rebuild_rows` re-derives `rows`
        // from) stale. A later collapse/expand action would then silently
        // revert the just-applied status change and miscompute
        // `direct_summary`'s complete/total counts.
        let mut core = core();
        let parent = core
            .create_task(minimal_new_task("Parent"))
            .expect("create_task should succeed");
        let child = core
            .create_task(bala_core::NewTask {
                parent_id: Some(parent.id),
                ..minimal_new_task("Child")
            })
            .expect("create_task should succeed");
        let tasks = vec![parent.clone(), child.clone()];
        let rows = crate::render::task_rows(
            &tasks,
            &crate::render::order_of(&tasks),
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        );
        let mut app = App::new(rows).with_tasks(tasks);
        app.select_by_id(Some(child.id));
        let _ = apply_action(&mut app, &mut core, Action::ToggleComplete);
        assert_eq!(
            app.rows()
                .iter()
                .find(|row| row.id == child.id)
                .map(|row| row.status),
            Some(TaskStatus::Complete)
        );

        // Trigger a rebuild_rows via collapse/expand of the parent, which
        // re-derives `rows` from `app.tasks`.
        app.select_by_id(Some(parent.id));
        let _ = apply_action(&mut app, &mut core, Action::CollapseFocused);

        let parent_row = app
            .rows()
            .iter()
            .find(|row| row.id == parent.id)
            .expect("parent row should still be present");
        assert_eq!(parent_row.direct_summary, Some((1, 1)));

        let _ = apply_action(&mut app, &mut core, Action::ExpandFocused);
        assert_eq!(
            app.rows()
                .iter()
                .find(|row| row.id == child.id)
                .map(|row| row.status),
            Some(TaskStatus::Complete)
        );
    }

    #[test]
    fn rebuild_rows_after_delete_should_not_resurrect_deleted_task() {
        // Regression: the delete path (`confirm_delete`) used to filter only
        // `app.rows`, leaving the deleted task(s) in `app.tasks`. A later
        // collapse/expand action would then re-derive `rows` from the stale
        // `app.tasks` and resurrect the deleted row(s).
        let mut core = core();
        let parent = core
            .create_task(minimal_new_task("Parent"))
            .expect("create_task should succeed");
        let child = core
            .create_task(bala_core::NewTask {
                parent_id: Some(parent.id),
                ..minimal_new_task("Child")
            })
            .expect("create_task should succeed");
        let tasks = vec![parent.clone(), child.clone()];
        let rows = crate::render::task_rows(
            &tasks,
            &crate::render::order_of(&tasks),
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        );
        let mut app = App::new(rows).with_tasks(tasks);
        app.select_by_id(Some(child.id));
        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);
        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);
        let _ = apply_action(&mut app, &mut core, Action::ConfirmYes);
        assert!(!app.rows().iter().any(|row| row.id == child.id));

        // Trigger a rebuild_rows via expand-all, which re-derives `rows`
        // from `app.tasks`.
        let _ = apply_action(&mut app, &mut core, Action::ExpandAll);

        assert!(!app.rows().iter().any(|row| row.id == child.id));
        assert!(app.rows().iter().any(|row| row.id == parent.id));
    }

    #[test]
    fn rebuild_rows_after_edit_title_should_not_revert_to_the_stale_cached_title() {
        // Regression: `submit_edit_title` used to patch only the matching
        // `TaskRow`, leaving `app.tasks` (the cache `rebuild_rows`
        // re-derives `rows` from) holding the old title. A later
        // collapse/expand action would then silently revert the just-edited
        // title back to its pre-edit value.
        let mut core = core();
        let parent = core
            .create_task(minimal_new_task("Parent"))
            .expect("create_task should succeed");
        let child = core
            .create_task(bala_core::NewTask {
                parent_id: Some(parent.id),
                ..minimal_new_task("Child")
            })
            .expect("create_task should succeed");
        let tasks = vec![parent.clone(), child.clone()];
        let rows = crate::render::task_rows(
            &tasks,
            &crate::render::order_of(&tasks),
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        );
        let mut app = App::new(rows).with_tasks(tasks);
        app.select_by_id(Some(parent.id));
        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);
        let _ = apply_action(&mut app, &mut core, Action::StartEditTitle);
        for _ in 0.."Parent".chars().count() {
            let _ = apply_action(&mut app, &mut core, Action::Backspace);
        }
        for c in "Renamed parent".chars() {
            let _ = apply_action(&mut app, &mut core, Action::InsertChar(c));
        }
        let _ = apply_action(&mut app, &mut core, Action::SubmitInsert);
        assert_eq!(
            app.rows()
                .iter()
                .find(|row| row.id == parent.id)
                .map(|row| row.title.as_str()),
            Some("Renamed parent")
        );

        // Trigger a rebuild_rows via collapse/expand, which re-derives
        // `rows` from `app.tasks`.
        let _ = apply_action(&mut app, &mut core, Action::CollapseFocused);
        let _ = apply_action(&mut app, &mut core, Action::ExpandFocused);

        assert_eq!(
            app.rows()
                .iter()
                .find(|row| row.id == parent.id)
                .map(|row| row.title.as_str()),
            Some("Renamed parent")
        );
    }

    #[test]
    fn collapse_all_should_reselect_nearest_visible_ancestor_when_focused_row_becomes_hidden() {
        // Regression: `CollapseAll` can hide the currently-focused row (when
        // some ancestor above it also has children and gets collapsed too),
        // unlike `CollapseFocused`/`ExpandFocused`, which only ever act on
        // the focused row itself. `rebuild_rows` used to leave `app.selected`
        // as a stale numeric index into the old `rows` in that case, which
        // could highlight (and let a later action target) an unrelated task.
        let mut core = core();
        let root = core
            .create_task(minimal_new_task("Root"))
            .expect("create_task should succeed");
        let parent = core
            .create_task(bala_core::NewTask {
                parent_id: Some(root.id),
                ..minimal_new_task("Parent")
            })
            .expect("create_task should succeed");
        let child = core
            .create_task(bala_core::NewTask {
                parent_id: Some(parent.id),
                ..minimal_new_task("Child")
            })
            .expect("create_task should succeed");
        let tasks = vec![root.clone(), parent.clone(), child.clone()];
        let rows = crate::render::task_rows(
            &tasks,
            &crate::render::order_of(&tasks),
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        );
        let mut app = App::new(rows).with_tasks(tasks);
        app.select_by_id(Some(child.id));

        let _ = apply_action(&mut app, &mut core, Action::CollapseAll);

        // Only `root` remains visible (its own subtree, including `parent`,
        // is hidden since `root` itself is collapsed).
        assert_eq!(app.rows().len(), 1);
        assert_eq!(app.rows()[0].id, root.id);
        assert_eq!(app.selected_row().map(|row| row.id), Some(root.id));
    }

    #[test]
    fn cycle_type_filter_should_advance_none_through_types_and_back_to_none() {
        let mut app = App::new(vec![])
            .with_type_filter_state(None, vec!["goal".to_string(), "task".to_string()]);
        assert_eq!(app.type_filter(), None);

        super::cycle_type_filter(&mut app);
        assert_eq!(app.type_filter(), Some("goal"));

        super::cycle_type_filter(&mut app);
        assert_eq!(app.type_filter(), Some("task"));

        super::cycle_type_filter(&mut app);
        assert_eq!(app.type_filter(), None);
    }

    #[test]
    fn cycle_type_filter_should_wrap_to_none_when_current_value_is_no_longer_in_the_list() {
        // Defensive case: a persisted/selected filter value that's since been
        // removed from the configured type list shouldn't get the cycle
        // stuck — treat "not found" the same as "was last".
        let mut app = App::new(vec![])
            .with_type_filter_state(Some("gone".to_string()), vec!["task".to_string()]);

        super::cycle_type_filter(&mut app);

        assert_eq!(app.type_filter(), None);
    }

    #[test]
    fn refresh_rows_from_tree_should_only_include_rows_matching_the_type_filter() {
        let mut core = core();
        core.upsert_task_type(bala_core::TaskType {
            key: "goal".to_string(),
            label: "Goal".to_string(),
            color: None,
            sort_order: 1,
        })
        .expect("upsert_task_type should succeed");
        let goal_task = core
            .create_task(bala_core::NewTask {
                type_key: Some("goal".to_string()),
                ..minimal_new_task("A goal")
            })
            .expect("create_task should succeed");
        let _plain_task = core
            .create_task(minimal_new_task("A plain task"))
            .expect("create_task should succeed");
        let mut app = App::new(vec![])
            .with_type_filter_state(Some("goal".to_string()), vec!["goal".to_string()]);

        super::refresh_rows_from_tree(&mut app, &mut core, None);

        assert_eq!(app.rows().len(), 1);
        assert_eq!(app.rows()[0].id, goal_task.id);
    }

    #[test]
    fn refresh_rows_from_tree_should_populate_descriptions_for_newly_visible_tasks() {
        let mut core = core();
        core.upsert_task_type(bala_core::TaskType {
            key: "goal".to_string(),
            label: "Goal".to_string(),
            color: None,
            sort_order: 1,
        })
        .expect("upsert_task_type should succeed");
        let goal_task = core
            .create_task(bala_core::NewTask {
                description: Some("important note".to_string()),
                type_key: Some("goal".to_string()),
                ..minimal_new_task("A goal")
            })
            .expect("create_task should succeed");
        // Start filtered to exclude the goal task, so it's never made it into
        // `app.descriptions` — mirrors a real session where the TUI launched
        // with a persisted `filter_type_key` narrower than "everything".
        let mut app = App::new(vec![])
            .with_type_filter_state(Some("task".to_string()), vec!["task".to_string()]);
        super::refresh_rows_from_tree(&mut app, &mut core, None);
        assert!(app.description_of(goal_task.id).is_none());

        // Cycling (or otherwise clearing) the filter reveals the goal task —
        // its description must come along, not silently read back as "none"
        // the next time it's edited.
        app.type_filter = None;
        super::refresh_rows_from_tree(&mut app, &mut core, None);

        assert_eq!(app.description_of(goal_task.id), Some("important note"));
    }

    #[test]
    fn refresh_rows_from_tree_should_select_first_row_when_selected_task_is_filtered_out() {
        let mut core = core();
        core.upsert_task_type(bala_core::TaskType {
            key: "goal".to_string(),
            label: "Goal".to_string(),
            color: None,
            sort_order: 1,
        })
        .expect("upsert_task_type should succeed");
        let goal_task = core
            .create_task(bala_core::NewTask {
                type_key: Some("goal".to_string()),
                ..minimal_new_task("Goal task")
            })
            .expect("create_task should succeed");
        let plain_task = core
            .create_task(minimal_new_task("Plain task"))
            .expect("create_task should succeed");
        // `goal_task` sits at index 1 so a stale, un-reset `selected` (left
        // over from a longer row list) would point past the end of the
        // single-row result below, not merely at the wrong-but-in-bounds row.
        let mut app = App::new(vec![row_for(&plain_task), row_for(&goal_task)]);
        app.selected = Some(1);

        // Simulate cycling the filter to "task": the previously selected
        // task (goal_task, at stale index 1) is no longer in the result set.
        app.type_filter = Some("task".to_string());
        super::refresh_rows_from_tree(&mut app, &mut core, Some(goal_task.id));

        assert_eq!(app.rows().len(), 1);
        assert_eq!(app.selected_index(), Some(0));
        assert_eq!(app.selected_row().unwrap().id, plain_task.id);
    }

    #[test]
    fn refresh_rows_from_tree_should_clear_selection_when_filter_leaves_no_rows() {
        let mut core = core();
        core.upsert_task_type(bala_core::TaskType {
            key: "goal".to_string(),
            label: "Goal".to_string(),
            color: None,
            sort_order: 1,
        })
        .expect("upsert_task_type should succeed");
        let goal_task = core
            .create_task(bala_core::NewTask {
                type_key: Some("goal".to_string()),
                ..minimal_new_task("Goal task")
            })
            .expect("create_task should succeed");
        let mut app = App::new(vec![row_for(&goal_task)]);

        app.type_filter = Some("nonexistent".to_string());
        super::refresh_rows_from_tree(&mut app, &mut core, Some(goal_task.id));

        assert_eq!(app.rows(), []);
        assert_eq!(app.selected_index(), None);
    }

    /// Seeds `First` and `Second`, then `Waiting` depending on both in that
    /// order; rows come out in creation order.
    fn two_predecessor_fixture() -> (Core<InMemoryStore>, App, [bala_core::Task; 3]) {
        let mut core = core();
        let first = core.create_task(minimal_new_task("First")).unwrap();
        let second = core.create_task(minimal_new_task("Second")).unwrap();
        let waiting = core.create_task(minimal_new_task("Waiting")).unwrap();
        for pred in [&first, &second] {
            core.add_dependency(waiting.id, pred.id, bala_core::DependencyType::default())
                .unwrap();
        }
        let app = app_from_core(&mut core);
        (core, app, [first, second, waiting])
    }

    fn related_task(title: &str, status: TaskStatus) -> super::RelatedTask {
        super::RelatedTask {
            title: title.to_string(),
            status,
        }
    }

    #[test]
    fn selecting_a_task_should_load_all_its_live_predecessors() {
        let (mut core, mut app, [_, second, _]) = two_predecessor_fixture();
        // One predecessor is already finished before the row is reached.
        core.complete_task(second.id, false).unwrap();
        super::refresh_rows_from_tree(&mut app, &mut core, None);
        assert_eq!(app.predecessors(), []);

        // The list is loaded once the selected row's Detail pane opens.
        let _ = apply_action(&mut app, &mut core, Action::MoveDown);
        let _ = apply_action(&mut app, &mut core, Action::MoveDown);
        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);

        assert_eq!(
            app.predecessors(),
            [
                related_task("First", TaskStatus::Incomplete),
                related_task("Second", TaskStatus::Complete),
            ]
        );
    }

    #[test]
    fn predecessors_should_not_be_loaded_while_the_list_pane_is_showing() {
        let (mut core, mut app, [_, _, waiting]) = two_predecessor_fixture();

        // Moving onto `Waiting` reloads the lists for it in the List pane.
        let _ = apply_action(&mut app, &mut core, Action::MoveDown);
        let _ = apply_action(&mut app, &mut core, Action::MoveDown);
        assert_eq!(app.selected_row().map(|r| r.id), Some(waiting.id));
        assert_eq!(app.predecessors(), []);

        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);
        assert_eq!(app.predecessors().len(), 2);

        let _ = apply_action(&mut app, &mut core, Action::LeaveDetail);
        assert_eq!(app.pane(), Pane::List);
        assert_eq!(app.predecessors(), []);
    }

    #[test]
    fn completed_predecessor_should_stay_listed_as_complete() {
        let (mut core, mut app, [_, _, waiting]) = two_predecessor_fixture();
        let _ = apply_action(&mut app, &mut core, Action::MoveDown);
        let _ = apply_action(&mut app, &mut core, Action::MoveDown);
        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);
        assert_eq!(app.predecessors().len(), 2);

        // Complete the first predecessor, then come back to its successor.
        let _ = apply_action(&mut app, &mut core, Action::LeaveDetail);
        let _ = apply_action(&mut app, &mut core, Action::MoveUp);
        let _ = apply_action(&mut app, &mut core, Action::MoveUp);
        let _ = apply_action(&mut app, &mut core, Action::ToggleComplete);
        let _ = apply_action(&mut app, &mut core, Action::MoveDown);
        let _ = apply_action(&mut app, &mut core, Action::MoveDown);
        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);

        assert_eq!(app.selected_row().map(|r| r.id), Some(waiting.id));
        assert_eq!(
            app.predecessors(),
            [
                related_task("First", TaskStatus::Complete),
                related_task("Second", TaskStatus::Incomplete),
            ]
        );
    }

    #[test]
    fn soft_deleted_predecessor_should_not_be_listed() {
        let (mut core, mut app, [first, _, waiting]) = two_predecessor_fixture();
        core.delete_task(first.id, bala_core::DeleteMode::Subtree)
            .unwrap();
        super::refresh_rows_from_tree(&mut app, &mut core, Some(waiting.id));
        // The deleted task's edge is still recorded on its successor.
        let stored = core.get_task(waiting.id).unwrap().unwrap();
        assert_eq!(stored.depends_on.len(), 2);

        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);

        assert_eq!(app.selected_row().map(|r| r.id), Some(waiting.id));
        assert_eq!(
            app.predecessors(),
            [related_task("Second", TaskStatus::Incomplete)]
        );
    }

    #[test]
    fn predecessor_hidden_by_the_type_filter_should_still_be_listed() {
        let mut core = core();
        core.upsert_task_type(TaskType {
            key: "goal".to_string(),
            label: "Goal".to_string(),
            color: None,
            sort_order: 1,
        })
        .unwrap();
        let plain = core.create_task(minimal_new_task("Plain")).unwrap();
        let goal = core
            .create_task(bala_core::NewTask {
                type_key: Some("goal".to_string()),
                ..minimal_new_task("Goal")
            })
            .unwrap();
        core.add_dependency(goal.id, plain.id, bala_core::DependencyType::default())
            .unwrap();
        core.complete_task(plain.id, false).unwrap();
        let mut app = app_from_core(&mut core)
            .with_type_filter_state(Some("goal".to_string()), vec!["goal".to_string()]);
        super::refresh_rows_from_tree(&mut app, &mut core, Some(goal.id));
        assert_eq!(titles(&app), ["Goal"]);

        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);

        assert_eq!(
            app.predecessors(),
            [related_task("Plain", TaskStatus::Complete)]
        );
    }

    #[test]
    fn detail_pane_should_load_the_tasks_that_depend_on_the_selected_task() {
        let mut core = core();
        let a = core.create_task(minimal_new_task("A")).unwrap();
        let b = core.create_task(minimal_new_task("B")).unwrap();
        let c = core.create_task(minimal_new_task("C")).unwrap();
        let _unrelated = core.create_task(minimal_new_task("Unrelated")).unwrap();
        // `C` is finished before it comes to depend on `A`.
        core.complete_task(c.id, false).unwrap();
        depend_on(&mut core, b.id, a.id);
        depend_on(&mut core, c.id, a.id);
        let mut app = app_from_core(&mut core);
        app.select_by_id(Some(a.id));

        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);

        assert_eq!(
            app.dependents(),
            [
                related_task("B", TaskStatus::Incomplete),
                related_task("C", TaskStatus::Complete),
            ]
        );
    }

    #[test]
    fn dependent_hidden_by_the_type_filter_should_still_be_listed() {
        let mut core = core();
        core.upsert_task_type(TaskType {
            key: "goal".to_string(),
            label: "Goal".to_string(),
            color: None,
            sort_order: 1,
        })
        .unwrap();
        let goal = core
            .create_task(bala_core::NewTask {
                type_key: Some("goal".to_string()),
                ..minimal_new_task("Goal")
            })
            .unwrap();
        let plain = core.create_task(minimal_new_task("Plain")).unwrap();
        depend_on(&mut core, plain.id, goal.id);
        let mut app = app_from_core(&mut core)
            .with_type_filter_state(Some("goal".to_string()), vec!["goal".to_string()]);
        super::refresh_rows_from_tree(&mut app, &mut core, Some(goal.id));
        assert_eq!(titles(&app), ["Goal"]);

        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);

        assert_eq!(
            app.dependents(),
            [related_task("Plain", TaskStatus::Incomplete)]
        );
    }

    #[test]
    fn dependents_should_not_be_loaded_while_the_list_pane_is_showing() {
        let mut core = core();
        let a = core.create_task(minimal_new_task("A")).unwrap();
        let b = core.create_task(minimal_new_task("B")).unwrap();
        depend_on(&mut core, b.id, a.id);
        let mut app = app_from_core(&mut core);
        app.select_by_id(Some(a.id));

        // Moving away and back reloads the lists for `A` in the List pane.
        let _ = apply_action(&mut app, &mut core, Action::MoveDown);
        let _ = apply_action(&mut app, &mut core, Action::MoveUp);
        assert_eq!(app.selected_row().map(|r| r.id), Some(a.id));
        assert_eq!(app.dependents(), []);

        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);
        assert_eq!(app.dependents().len(), 1);

        let _ = apply_action(&mut app, &mut core, Action::LeaveDetail);
        assert_eq!(app.pane(), Pane::List);
        assert_eq!(app.dependents(), []);
    }

    #[test]
    fn dependents_should_reload_after_a_dependency_is_added_or_removed() {
        let mut core = core();
        let a = core.create_task(minimal_new_task("A")).unwrap();
        let b = core.create_task(minimal_new_task("B")).unwrap();
        let mut app = app_from_core(&mut core);
        app.select_by_id(Some(a.id));
        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);
        assert_eq!(app.dependents(), []);

        // The edge is edited on the dependent's row, so leave `A`, make `B`
        // depend on it, and come back.
        let _ = apply_action(&mut app, &mut core, Action::LeaveDetail);
        submit_dependency_entry(&mut app, &mut core, b.id, &id_text(a.id));
        app.select_by_id(Some(a.id));
        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);
        assert_eq!(
            app.dependents(),
            [related_task("B", TaskStatus::Incomplete)]
        );

        // The remove line is prefilled with `B`'s only predecessor.
        let _ = apply_action(&mut app, &mut core, Action::LeaveDetail);
        submit_entry(&mut app, &mut core, b.id, Action::StartRemoveDependency, "");
        app.select_by_id(Some(a.id));
        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);
        assert_eq!(app.dependents(), []);
    }

    #[test]
    fn reload_dependents_should_set_error_when_the_unfiltered_tree_cannot_be_read() {
        let remaining = std::rc::Rc::new(std::cell::Cell::new(None));
        let mut core = Core::new(FlakyStore {
            inner: InMemoryStore::default(),
            remaining: std::rc::Rc::clone(&remaining),
        })
        .expect("core should construct");
        core.upsert_task_type(TaskType {
            key: "goal".to_string(),
            label: "Goal".to_string(),
            color: None,
            sort_order: 1,
        })
        .expect("upsert type");
        let goal = core
            .create_task(bala_core::NewTask {
                type_key: Some("goal".to_string()),
                ..minimal_new_task("Goal")
            })
            .expect("create");
        let plain = core.create_task(minimal_new_task("Plain")).expect("create");
        core.add_dependency(plain.id, goal.id, bala_core::DependencyType::default())
            .expect("add dependency");
        let tasks = core
            .get_tree(bala_core::TreeFilter {
                type_key: Some("goal".to_string()),
                ..Default::default()
            })
            .expect("get_tree should succeed");
        let order = core.sibling_order().expect("sibling_order should succeed");
        let rows = crate::render::task_rows(
            &tasks,
            &order,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
        );
        let mut app = App::new(rows)
            .with_tasks(tasks)
            .with_sibling_order(order)
            .with_type_filter_state(Some("goal".to_string()), vec!["goal".to_string()]);
        app.select_by_id(Some(goal.id));
        remaining.set(Some(0));

        // In the List pane the unfiltered tree is never fetched.
        super::reload_dependents(&mut app, &core);
        assert_eq!(app.error(), None);

        app.pane = Pane::Detail;
        super::reload_dependents(&mut app, &core);

        assert!(app.error().is_some());
        assert_eq!(app.dependents(), []);

        // An error already shown is kept.
        app.error = Some("original".to_string());
        super::reload_dependents(&mut app, &core);
        assert_eq!(app.error(), Some("original"));
    }

    /// Seeds `Blocker` (incomplete, blocks `Blocked`), `Blocked`, `Free`
    /// (neither) and `Done` (complete), all untyped.
    fn blocked_view_fixture() -> (Core<InMemoryStore>, App) {
        let mut core = core();
        let pred = core.create_task(minimal_new_task("Blocker")).unwrap();
        let waiting = core.create_task(minimal_new_task("Blocked")).unwrap();
        core.create_task(minimal_new_task("Free")).unwrap();
        let done = core.create_task(minimal_new_task("Done")).unwrap();
        core.add_dependency(waiting.id, pred.id, bala_core::DependencyType::default())
            .unwrap();
        core.complete_task(done.id, false).unwrap();
        let app = app_from_core(&mut core);
        (core, app)
    }

    #[test]
    fn cycle_blocked_view_should_step_all_blocked_ready_all() {
        let (mut core, mut app) = blocked_view_fixture();
        assert_eq!(app.blocked_view(), super::BlockedView::All);

        let _ = apply_action(&mut app, &mut core, Action::CycleBlockedView);
        assert_eq!(app.blocked_view(), super::BlockedView::Blocked);
        let _ = apply_action(&mut app, &mut core, Action::CycleBlockedView);
        assert_eq!(app.blocked_view(), super::BlockedView::Ready);
        let _ = apply_action(&mut app, &mut core, Action::CycleBlockedView);
        assert_eq!(app.blocked_view(), super::BlockedView::All);
    }

    #[test]
    fn blocked_view_should_show_only_tasks_with_blocked_by() {
        let (mut core, mut app) = blocked_view_fixture();

        let _ = apply_action(&mut app, &mut core, Action::CycleBlockedView);

        assert_eq!(titles(&app), ["Blocked"]);
        assert!(app.rows()[0].blocked);
        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);
        assert_eq!(app.predecessors().len(), 1);
    }

    #[test]
    fn ready_view_should_show_only_incomplete_unblocked_tasks() {
        let (mut core, mut app) = blocked_view_fixture();

        let _ = apply_action(&mut app, &mut core, Action::CycleBlockedView);
        let _ = apply_action(&mut app, &mut core, Action::CycleBlockedView);

        assert_eq!(titles(&app), ["Blocker", "Free"]);
        assert_eq!(
            app.selected_row().map(|r| r.title.as_str()),
            Some("Blocker")
        );
    }

    #[test]
    fn blocked_view_should_compose_with_the_type_filter() {
        let mut core = core();
        core.upsert_task_type(TaskType {
            key: "goal".to_string(),
            label: "Goal".to_string(),
            color: None,
            sort_order: 1,
        })
        .unwrap();
        let blocker = core.create_task(minimal_new_task("Blocker")).unwrap();
        let blocked_goal = core
            .create_task(bala_core::NewTask {
                type_key: Some("goal".to_string()),
                ..minimal_new_task("Blocked goal")
            })
            .unwrap();
        let blocked_plain = core.create_task(minimal_new_task("Blocked plain")).unwrap();
        for id in [blocked_goal.id, blocked_plain.id] {
            core.add_dependency(id, blocker.id, bala_core::DependencyType::default())
                .unwrap();
        }
        let mut app = app_from_core(&mut core)
            .with_type_filter_state(Some("goal".to_string()), vec!["goal".to_string()]);

        let _ = apply_action(&mut app, &mut core, Action::CycleBlockedView);

        assert_eq!(titles(&app), ["Blocked goal"]);
    }

    #[test]
    fn blocked_view_should_render_a_child_of_a_filtered_out_parent_at_top_level() {
        let mut core = core();
        let parent = core.create_task(minimal_new_task("Parent")).unwrap();
        let child = child_of(&mut core, "Child", parent.id);
        let blocker = core.create_task(minimal_new_task("Blocker")).unwrap();
        core.add_dependency(child.id, blocker.id, bala_core::DependencyType::default())
            .unwrap();
        let mut app = app_from_core(&mut core);

        let _ = apply_action(&mut app, &mut core, Action::CycleBlockedView);

        assert_eq!(titles(&app), ["Child"]);
        assert_eq!(app.rows()[0].depth, 0);
    }

    #[test]
    fn blocked_view_should_keep_selection_valid_when_the_selected_row_is_filtered_out() {
        let (mut core, mut app) = blocked_view_fixture();
        let _ = apply_action(&mut app, &mut core, Action::MoveDown);
        let _ = apply_action(&mut app, &mut core, Action::MoveDown);
        let _ = apply_action(&mut app, &mut core, Action::MoveDown);
        assert_eq!(app.selected_row().map(|r| r.title.as_str()), Some("Done"));

        let _ = apply_action(&mut app, &mut core, Action::CycleBlockedView);

        assert_eq!(
            app.selected_row().map(|r| r.title.as_str()),
            Some("Blocked")
        );
    }

    #[test]
    fn blocked_view_should_persist_across_a_tree_refresh() {
        let (mut core, mut app) = blocked_view_fixture();
        let _ = apply_action(&mut app, &mut core, Action::CycleBlockedView);

        // Completing the only blocker unblocks "Blocked"; the next refresh
        // keeps the Blocked view and now lists nothing.
        let _ = apply_action(&mut app, &mut core, Action::MoveDown);
        super::refresh_rows_from_tree(&mut app, &mut core, None);
        assert_eq!(titles(&app), ["Blocked"]);
        let blocker = app.tasks.iter().find(|t| t.title == "Blocker").unwrap().id;
        core.complete_task(blocker, false).unwrap();
        super::refresh_rows_from_tree(&mut app, &mut core, None);

        assert_eq!(app.rows(), []);
        assert_eq!(app.blocked_view(), super::BlockedView::Blocked);
    }

    /// The id of the task titled `title` in `app`'s cached tree.
    fn id_of(app: &App, title: &str) -> TaskId {
        app.tasks
            .iter()
            .find(|task| task.title == title)
            .unwrap_or_else(|| panic!("no cached task titled {title}"))
            .id
    }

    #[test]
    fn adding_a_dependency_in_detail_should_return_to_the_list_when_the_ready_view_hides_the_task()
    {
        let (mut core, mut app) = blocked_view_fixture();
        let (blocker, free) = (id_of(&app, "Blocker"), id_of(&app, "Free"));
        let _ = apply_action(&mut app, &mut core, Action::CycleBlockedView);
        let _ = apply_action(&mut app, &mut core, Action::CycleBlockedView);
        assert_eq!(titles(&app), ["Blocker", "Free"]);
        app.select_by_id(Some(free));
        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);

        submit_dependency_entry(&mut app, &mut core, free, &id_text(blocker));

        assert_eq!(titles(&app), ["Blocker"]);
        assert_eq!(app.pane(), Pane::List);
        assert_eq!(app.mode(), &Mode::Normal);
        assert_eq!(app.selected_row().map(|r| r.id), Some(blocker));
    }

    #[test]
    fn removing_a_dependency_in_detail_should_return_to_the_list_when_the_blocked_view_hides_the_task()
     {
        let (mut core, mut app) = blocked_view_fixture();
        let blocked = id_of(&app, "Blocked");
        let _ = apply_action(&mut app, &mut core, Action::CycleBlockedView);
        assert_eq!(titles(&app), ["Blocked"]);
        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);

        // The line opens holding the only predecessor's id.
        submit_entry(
            &mut app,
            &mut core,
            blocked,
            Action::StartRemoveDependency,
            "",
        );

        assert_eq!(app.rows(), []);
        assert_eq!(app.pane(), Pane::List);
        assert_eq!(app.mode(), &Mode::Normal);
        assert_eq!(app.selected_row(), None);
    }

    #[test]
    fn completing_a_task_in_detail_should_return_to_the_list_when_the_ready_view_hides_it() {
        let (mut core, mut app) = blocked_view_fixture();
        let (blocker, free) = (id_of(&app, "Blocker"), id_of(&app, "Free"));
        let _ = apply_action(&mut app, &mut core, Action::CycleBlockedView);
        let _ = apply_action(&mut app, &mut core, Action::CycleBlockedView);
        app.select_by_id(Some(free));
        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);

        let _ = apply_action(&mut app, &mut core, Action::ToggleComplete);

        assert_eq!(titles(&app), ["Blocker"]);
        assert_eq!(app.pane(), Pane::List);
        assert_eq!(app.selected_row().map(|r| r.id), Some(blocker));
    }

    #[test]
    fn adding_a_dependency_in_detail_should_stay_on_the_task_while_it_is_still_listed() {
        let (mut core, mut app) = blocked_view_fixture();
        let (blocker, free) = (id_of(&app, "Blocker"), id_of(&app, "Free"));
        app.select_by_id(Some(free));
        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);

        submit_dependency_entry(&mut app, &mut core, free, &id_text(blocker));

        assert_eq!(app.pane(), Pane::Detail);
        assert_eq!(app.mode(), &Mode::Normal);
        assert_eq!(app.selected_row().map(|r| r.id), Some(free));
        assert_eq!(
            app.predecessors(),
            [related_task("Blocker", TaskStatus::Incomplete)]
        );
    }

    /// Types `id` into the open entry line, one `InsertChar` per character.
    fn type_task_id(app: &mut App, core: &mut Core<InMemoryStore>, id: TaskId) {
        for c in uuid::Uuid::from(id).to_string().chars() {
            let _ = apply_action(app, core, Action::InsertChar(c));
        }
    }

    #[test]
    fn start_add_dependency_should_open_an_empty_entry_line_for_the_selected_task() {
        let mut core = core();
        let _first = core.create_task(minimal_new_task("A")).unwrap();
        let second = core.create_task(minimal_new_task("B")).unwrap();
        let mut app = app_from_core(&mut core);
        app.select_by_id(Some(second.id));

        let _ = apply_action(&mut app, &mut core, Action::StartAddDependency);

        assert_eq!(
            app.mode(),
            &Mode::Insert {
                field: EditableField::AddPredecessor(second.id),
                buffer: String::new(),
            }
        );
    }

    #[test]
    fn start_add_dependency_with_no_selection_should_be_noop() {
        let mut core = core();
        let mut app = App::new(Vec::new());

        let _ = apply_action(&mut app, &mut core, Action::StartAddDependency);

        assert_eq!(app.mode(), &Mode::Normal);
        assert_eq!(app.error(), None);
    }

    #[test]
    fn submit_add_dependency_should_call_add_dependency_and_return_to_normal() {
        let mut core = core();
        let pred = core.create_task(minimal_new_task("A")).unwrap();
        let task = core.create_task(minimal_new_task("B")).unwrap();
        let mut app = app_from_core(&mut core);
        app.select_by_id(Some(task.id));
        let _ = apply_action(&mut app, &mut core, Action::StartAddDependency);
        type_task_id(&mut app, &mut core, pred.id);

        let _ = apply_action(&mut app, &mut core, Action::SubmitInsert);

        assert_eq!(app.mode(), &Mode::Normal);
        assert_eq!(app.error(), None);
        let updated = core.get_task(task.id).unwrap().expect("B should exist");
        assert_eq!(
            updated.depends_on,
            [bala_core::Dependency {
                predecessor_id: pred.id,
                dep_type: bala_core::DependencyType::FinishToStart,
            }]
        );
    }

    #[test]
    fn submit_add_dependency_twice_should_give_the_task_two_predecessors() {
        let mut core = core();
        let first = core.create_task(minimal_new_task("A")).unwrap();
        let second = core.create_task(minimal_new_task("B")).unwrap();
        let task = core.create_task(minimal_new_task("C")).unwrap();
        let mut app = app_from_core(&mut core);
        app.select_by_id(Some(task.id));

        for pred in [&first, &second] {
            let _ = apply_action(&mut app, &mut core, Action::StartAddDependency);
            type_task_id(&mut app, &mut core, pred.id);
            let _ = apply_action(&mut app, &mut core, Action::SubmitInsert);
        }

        assert_eq!(app.mode(), &Mode::Normal);
        assert_eq!(app.error(), None);
        let updated = core.get_task(task.id).unwrap().expect("C should exist");
        let predecessors: HashSet<TaskId> = updated
            .depends_on
            .iter()
            .map(|dep| dep.predecessor_id)
            .collect();
        assert_eq!(predecessors, HashSet::from([first.id, second.id]));
    }

    #[test]
    fn submit_add_dependency_should_mark_the_row_blocked() {
        let mut core = core();
        let pred = core.create_task(minimal_new_task("A")).unwrap();
        let task = core.create_task(minimal_new_task("B")).unwrap();
        let mut app = app_from_core(&mut core);
        app.select_by_id(Some(task.id));
        assert_eq!(app.selected_row().map(|r| r.blocked), Some(false));
        let _ = apply_action(&mut app, &mut core, Action::StartAddDependency);
        type_task_id(&mut app, &mut core, pred.id);

        let _ = apply_action(&mut app, &mut core, Action::SubmitInsert);

        let selected = app.selected_row().expect("a row should be selected");
        assert_eq!(selected.id, task.id);
        assert!(selected.blocked);
    }

    /// Selects `task`, opens its dependency entry line, types `entry` and
    /// submits it.
    fn submit_dependency_entry(
        app: &mut App,
        core: &mut Core<InMemoryStore>,
        task: TaskId,
        entry: &str,
    ) {
        submit_entry(app, core, task, Action::StartAddDependency, entry);
    }

    /// Selects `task`, opens an entry line with `start`, types `entry` after
    /// whatever the line was prefilled with and submits it.
    fn submit_entry(
        app: &mut App,
        core: &mut Core<InMemoryStore>,
        task: TaskId,
        start: Action,
        entry: &str,
    ) {
        app.select_by_id(Some(task));
        let _ = apply_action(app, core, start);
        for c in entry.chars() {
            let _ = apply_action(app, core, Action::InsertChar(c));
        }
        let _ = apply_action(app, core, Action::SubmitInsert);
    }

    /// The remove-dependency entry line for `task`, still open and holding
    /// `entry`.
    fn remove_dependency_entry_line(task: TaskId, entry: &str) -> Mode {
        Mode::Insert {
            field: EditableField::RemovePredecessor(task),
            buffer: entry.to_string(),
        }
    }

    /// Makes `task` depend on `predecessor`, finish-to-start.
    fn depend_on(core: &mut Core<InMemoryStore>, task: TaskId, predecessor: TaskId) {
        core.add_dependency(task, predecessor, bala_core::DependencyType::FinishToStart)
            .expect("add_dependency should succeed");
    }

    /// The dependency entry line for `task`, still open and holding `entry`.
    fn dependency_entry_line(task: TaskId, entry: &str) -> Mode {
        Mode::Insert {
            field: EditableField::AddPredecessor(task),
            buffer: entry.to_string(),
        }
    }

    fn id_text(id: TaskId) -> String {
        uuid::Uuid::from(id).to_string()
    }

    #[test]
    fn submit_add_dependency_with_empty_line_should_show_inline_error() {
        let mut core = core();
        let task = core.create_task(minimal_new_task("A")).unwrap();
        let mut app = app_from_core(&mut core);

        submit_dependency_entry(&mut app, &mut core, task.id, "");

        assert_eq!(app.mode(), &dependency_entry_line(task.id, ""));
        assert_eq!(app.error(), Some("enter a task id"));
    }

    #[test]
    fn submit_add_dependency_with_invalid_uuid_should_show_inline_error() {
        let mut core = core();
        let task = core.create_task(minimal_new_task("A")).unwrap();
        let mut app = app_from_core(&mut core);

        submit_dependency_entry(&mut app, &mut core, task.id, "not-a-uuid");

        assert_eq!(app.mode(), &dependency_entry_line(task.id, "not-a-uuid"));
        assert_eq!(app.error(), Some("invalid task id"));
    }

    #[test]
    fn submit_add_dependency_with_more_than_one_id_should_show_inline_error() {
        let mut core = core();
        let first = core.create_task(minimal_new_task("A")).unwrap();
        let second = core.create_task(minimal_new_task("B")).unwrap();
        let task = core.create_task(minimal_new_task("C")).unwrap();
        let mut app = app_from_core(&mut core);
        let entry = format!("{}, {}", id_text(first.id), id_text(second.id));

        submit_dependency_entry(&mut app, &mut core, task.id, &entry);

        assert_eq!(app.mode(), &dependency_entry_line(task.id, &entry));
        assert_eq!(app.error(), Some("enter one task id at a time"));
        let stored = core.get_task(task.id).unwrap().expect("C should exist");
        assert_eq!(stored.depends_on, []);
    }

    #[test]
    fn submit_add_dependency_with_unknown_id_should_show_inline_error() {
        let mut core = core();
        let task = core.create_task(minimal_new_task("A")).unwrap();
        let mut app = app_from_core(&mut core);
        let unknown = "00000000-0000-4000-8000-000000000001";

        submit_dependency_entry(&mut app, &mut core, task.id, unknown);

        assert_eq!(app.mode(), &dependency_entry_line(task.id, unknown));
        assert_eq!(app.error(), Some("no task with that id"));
    }

    #[test]
    fn submit_add_dependency_on_a_vanished_task_should_say_the_task_is_gone() {
        let mut core = core();
        let a = core.create_task(minimal_new_task("A")).unwrap();
        let b = core.create_task(minimal_new_task("B")).unwrap();
        let mut app = app_from_core(&mut core);
        // Another writer deletes `B` while its row is still on screen.
        core.delete_task(b.id, bala_core::DeleteMode::Subtree)
            .unwrap();
        let entry = id_text(a.id);

        submit_dependency_entry(&mut app, &mut core, b.id, &entry);

        assert_eq!(app.mode(), &dependency_entry_line(b.id, &entry));
        assert_eq!(app.error(), Some("this task no longer exists"));
    }

    #[test]
    fn submit_remove_dependency_on_a_vanished_task_should_say_the_task_is_gone() {
        let mut core = core();
        let a = core.create_task(minimal_new_task("A")).unwrap();
        let b = core.create_task(minimal_new_task("B")).unwrap();
        depend_on(&mut core, b.id, a.id);
        let mut app = app_from_core(&mut core);
        app.select_by_id(Some(b.id));
        let _ = apply_action(&mut app, &mut core, Action::StartRemoveDependency);
        // Another writer deletes `B` while its entry line is open.
        core.delete_task(b.id, bala_core::DeleteMode::Subtree)
            .unwrap();
        let entry = id_text(a.id);

        let _ = apply_action(&mut app, &mut core, Action::SubmitInsert);

        assert_eq!(app.mode(), &remove_dependency_entry_line(b.id, &entry));
        assert_eq!(app.error(), Some("this task no longer exists"));
    }

    #[test]
    fn submit_add_dependency_on_itself_should_show_inline_error_and_stay_in_insert() {
        let mut core = core();
        let task = core.create_task(minimal_new_task("A")).unwrap();
        let mut app = app_from_core(&mut core);
        let entry = id_text(task.id);

        submit_dependency_entry(&mut app, &mut core, task.id, &entry);

        assert_eq!(app.mode(), &dependency_entry_line(task.id, &entry));
        assert_eq!(app.error(), Some("a task cannot depend on itself"));
    }

    #[test]
    fn submit_add_dependency_on_an_ancestor_should_name_it_by_title() {
        let mut core = core();
        let parent = core.create_task(minimal_new_task("Parent")).unwrap();
        let child = child_of(&mut core, "Child", parent.id);
        let grandchild = child_of(&mut core, "Grandchild", child.id);
        let mut app = app_from_core(&mut core);
        let entry = id_text(parent.id);

        submit_dependency_entry(&mut app, &mut core, grandchild.id, &entry);

        assert_eq!(app.mode(), &dependency_entry_line(grandchild.id, &entry));
        assert_eq!(
            app.error(),
            Some("\"Parent\" is an ancestor or descendant of this task")
        );
    }

    #[test]
    fn submit_add_dependency_on_a_descendant_should_name_it_by_title() {
        let mut core = core();
        let parent = core.create_task(minimal_new_task("Parent")).unwrap();
        let child = child_of(&mut core, "Child", parent.id);
        let grandchild = child_of(&mut core, "Grandchild", child.id);
        let mut app = app_from_core(&mut core);
        let entry = id_text(grandchild.id);

        submit_dependency_entry(&mut app, &mut core, parent.id, &entry);

        assert_eq!(app.mode(), &dependency_entry_line(parent.id, &entry));
        assert_eq!(
            app.error(),
            Some("\"Grandchild\" is an ancestor or descendant of this task")
        );
    }

    #[test]
    fn submit_add_dependency_closing_a_cycle_should_show_the_cycle_by_title() {
        let mut core = core();
        let a = core.create_task(minimal_new_task("A")).unwrap();
        let b = core.create_task(minimal_new_task("B")).unwrap();
        let c = core.create_task(minimal_new_task("C")).unwrap();
        core.add_dependency(b.id, a.id, bala_core::DependencyType::default())
            .unwrap();
        core.add_dependency(c.id, b.id, bala_core::DependencyType::default())
            .unwrap();
        let mut app = app_from_core(&mut core);

        submit_dependency_entry(&mut app, &mut core, a.id, &id_text(b.id));
        assert_eq!(app.error(), Some("would create a cycle: A → B → A"));

        let _ = apply_action(&mut app, &mut core, Action::CancelInsert);
        submit_dependency_entry(&mut app, &mut core, a.id, &id_text(c.id));
        assert_eq!(app.error(), Some("would create a cycle: A → C → B → A"));
    }

    #[test]
    fn submit_add_dependency_closing_a_cycle_should_name_a_task_hidden_by_the_type_filter() {
        let mut core = core();
        core.upsert_task_type(TaskType {
            key: "goal".to_string(),
            label: "Goal".to_string(),
            color: None,
            sort_order: 1,
        })
        .unwrap();
        let goal = core
            .create_task(bala_core::NewTask {
                type_key: Some("goal".to_string()),
                ..minimal_new_task("Goal")
            })
            .unwrap();
        let plain = core.create_task(minimal_new_task("Plain")).unwrap();
        depend_on(&mut core, plain.id, goal.id);
        let mut app = app_from_core(&mut core)
            .with_type_filter_state(Some("goal".to_string()), vec!["goal".to_string()]);
        super::refresh_rows_from_tree(&mut app, &mut core, Some(goal.id));
        assert_eq!(titles(&app), ["Goal"]);

        submit_dependency_entry(&mut app, &mut core, goal.id, &id_text(plain.id));

        assert_eq!(
            app.error(),
            Some("would create a cycle: Goal → Plain → Goal")
        );
    }

    #[test]
    fn rejected_add_dependency_should_fall_back_to_the_cached_tasks_when_the_tree_cannot_be_read() {
        let remaining = std::rc::Rc::new(std::cell::Cell::new(None));
        let mut core = Core::new(FlakyStore {
            inner: InMemoryStore::default(),
            remaining: std::rc::Rc::clone(&remaining),
        })
        .expect("core should construct");
        core.upsert_task_type(TaskType {
            key: "goal".to_string(),
            label: "Goal".to_string(),
            color: None,
            sort_order: 1,
        })
        .expect("upsert type");
        let goal = core
            .create_task(bala_core::NewTask {
                type_key: Some("goal".to_string()),
                ..minimal_new_task("Goal")
            })
            .expect("create");
        let plain = core.create_task(minimal_new_task("Plain")).expect("create");
        core.add_dependency(plain.id, goal.id, bala_core::DependencyType::default())
            .expect("add dependency");
        let mut app = App::new(vec![])
            .with_type_filter_state(Some("goal".to_string()), vec!["goal".to_string()]);
        super::refresh_rows_from_tree(&mut app, &mut core, Some(goal.id));
        let entry = id_text(plain.id);
        app.mode = dependency_entry_line(goal.id, &entry);
        // Reading the task and rejecting the edge succeed; the unfiltered
        // tree fetch after them fails.
        remaining.set(Some(2));

        let _ = apply_action(&mut app, &mut core, Action::SubmitInsert);

        assert_eq!(remaining.get(), Some(0));
        assert_eq!(
            app.error(),
            Some(format!("would create a cycle: Goal → {entry} → Goal").as_str())
        );
    }

    #[test]
    fn rejected_add_dependency_should_keep_the_buffer_and_write_no_edge() {
        let mut core = core();
        let a = core.create_task(minimal_new_task("A")).unwrap();
        let b = core.create_task(minimal_new_task("B")).unwrap();
        let edge = bala_core::Dependency {
            predecessor_id: a.id,
            dep_type: bala_core::DependencyType::FinishToStart,
        };
        core.add_dependency(b.id, a.id, edge.dep_type).unwrap();
        let mut app = app_from_core(&mut core);
        let entry = id_text(b.id);

        submit_dependency_entry(&mut app, &mut core, a.id, &entry);

        assert_eq!(app.mode(), &dependency_entry_line(a.id, &entry));
        assert!(app.error().is_some());
        let stored_a = core.get_task(a.id).unwrap().expect("A should exist");
        assert_eq!(stored_a.depends_on, []);
        let stored_b = core.get_task(b.id).unwrap().expect("B should exist");
        assert_eq!(stored_b.depends_on, [edge]);
    }

    #[test]
    fn submit_add_dependency_on_an_existing_predecessor_should_show_inline_error_and_write_nothing()
    {
        let mut core = core();
        let a = core.create_task(minimal_new_task("A")).unwrap();
        let b = core.create_task(minimal_new_task("B")).unwrap();
        depend_on(&mut core, b.id, a.id);
        let before = core.get_task(b.id).unwrap().expect("B should exist");
        let mut app = app_from_core(&mut core);
        let entry = id_text(a.id);

        submit_dependency_entry(&mut app, &mut core, b.id, &entry);

        assert_eq!(app.mode(), &dependency_entry_line(b.id, &entry));
        assert_eq!(app.error(), Some("this task already depends on that task"));
        let stored = core.get_task(b.id).unwrap().expect("B should exist");
        assert_eq!(stored, before);
    }

    #[test]
    fn submit_add_dependency_on_an_existing_predecessor_should_keep_the_edge_type() {
        let mut core = core();
        let a = core.create_task(minimal_new_task("A")).unwrap();
        let b = core.create_task(minimal_new_task("B")).unwrap();
        let edge = bala_core::Dependency {
            predecessor_id: a.id,
            dep_type: bala_core::DependencyType::StartToStart,
        };
        core.add_dependency(b.id, a.id, edge.dep_type).unwrap();
        let mut app = app_from_core(&mut core);

        submit_dependency_entry(&mut app, &mut core, b.id, &id_text(a.id));

        assert_eq!(app.error(), Some("this task already depends on that task"));
        let stored = core.get_task(b.id).unwrap().expect("B should exist");
        assert_eq!(stored.depends_on, [edge]);
    }

    #[test]
    fn start_remove_dependency_should_prefill_the_only_predecessor() {
        let mut core = core();
        let a = core.create_task(minimal_new_task("A")).unwrap();
        let b = core.create_task(minimal_new_task("B")).unwrap();
        depend_on(&mut core, b.id, a.id);
        let mut app = app_from_core(&mut core);
        app.select_by_id(Some(b.id));

        let _ = apply_action(&mut app, &mut core, Action::StartRemoveDependency);

        assert_eq!(
            app.mode(),
            &remove_dependency_entry_line(b.id, &id_text(a.id))
        );
        assert_eq!(app.error(), None);
    }

    #[test]
    fn start_remove_dependency_should_open_empty_with_several_predecessors() {
        let mut core = core();
        let a = core.create_task(minimal_new_task("A")).unwrap();
        let b = core.create_task(minimal_new_task("B")).unwrap();
        let c = core.create_task(minimal_new_task("C")).unwrap();
        depend_on(&mut core, c.id, a.id);
        depend_on(&mut core, c.id, b.id);
        let mut app = app_from_core(&mut core);
        app.select_by_id(Some(c.id));

        let _ = apply_action(&mut app, &mut core, Action::StartRemoveDependency);

        assert_eq!(app.mode(), &remove_dependency_entry_line(c.id, ""));
        assert_eq!(app.error(), None);
    }

    #[test]
    fn submit_remove_dependency_should_drop_the_predecessor_and_keep_the_others() {
        let mut core = core();
        let a = core.create_task(minimal_new_task("A")).unwrap();
        let b = core.create_task(minimal_new_task("B")).unwrap();
        let c = core.create_task(minimal_new_task("C")).unwrap();
        depend_on(&mut core, c.id, a.id);
        depend_on(&mut core, c.id, b.id);
        let mut app = app_from_core(&mut core);

        submit_entry(
            &mut app,
            &mut core,
            c.id,
            Action::StartRemoveDependency,
            &id_text(a.id),
        );

        assert_eq!(app.mode(), &Mode::Normal);
        assert_eq!(app.error(), None);
        let stored = core.get_task(c.id).unwrap().expect("C should exist");
        assert_eq!(
            stored.depends_on,
            [bala_core::Dependency {
                predecessor_id: b.id,
                dep_type: bala_core::DependencyType::FinishToStart,
            }]
        );
    }

    #[test]
    fn submit_remove_dependency_should_clear_the_blocked_flag_when_the_last_blocker_goes() {
        let mut core = core();
        let a = core.create_task(minimal_new_task("A")).unwrap();
        let b = core.create_task(minimal_new_task("B")).unwrap();
        depend_on(&mut core, b.id, a.id);
        let mut app = app_from_core(&mut core);
        super::refresh_rows_from_tree(&mut app, &mut core, Some(b.id));
        assert_eq!(app.selected_row().map(|r| r.blocked), Some(true));

        // The line opens holding A's id, so there is nothing to type.
        submit_entry(&mut app, &mut core, b.id, Action::StartRemoveDependency, "");

        assert_eq!(app.mode(), &Mode::Normal);
        let selected = app.selected_row().expect("a row should be selected");
        assert_eq!(selected.id, b.id);
        assert!(!selected.blocked);
    }

    #[test]
    fn submit_remove_dependency_on_a_task_it_does_not_depend_on_should_show_inline_error() {
        let mut core = core();
        let a = core.create_task(minimal_new_task("A")).unwrap();
        let b = core.create_task(minimal_new_task("B")).unwrap();
        let c = core.create_task(minimal_new_task("C")).unwrap();
        let other = core.create_task(minimal_new_task("D")).unwrap();
        depend_on(&mut core, c.id, a.id);
        depend_on(&mut core, c.id, b.id);
        let before = core.get_task(c.id).unwrap().expect("C should exist");
        let mut app = app_from_core(&mut core);
        let entry = id_text(other.id);

        submit_entry(
            &mut app,
            &mut core,
            c.id,
            Action::StartRemoveDependency,
            &entry,
        );

        assert_eq!(app.mode(), &remove_dependency_entry_line(c.id, &entry));
        assert_eq!(app.error(), Some("this task does not depend on that task"));
        let stored = core.get_task(c.id).unwrap().expect("C should exist");
        assert_eq!(stored.depends_on, before.depends_on);
    }

    #[test]
    fn submit_remove_dependency_with_invalid_uuid_should_show_inline_error() {
        let mut core = core();
        let task = core.create_task(minimal_new_task("A")).unwrap();
        let mut app = app_from_core(&mut core);

        submit_entry(
            &mut app,
            &mut core,
            task.id,
            Action::StartRemoveDependency,
            "not-a-uuid",
        );

        assert_eq!(
            app.mode(),
            &remove_dependency_entry_line(task.id, "not-a-uuid")
        );
        assert_eq!(app.error(), Some("invalid task id"));
    }

    #[test]
    fn dependency_error_message_should_use_the_plain_uuid_for_a_task_missing_from_tasks() {
        let mut core = core();
        let a = core.create_task(minimal_new_task("A")).unwrap();
        let hidden = core.create_task(minimal_new_task("Hidden")).unwrap();
        let err = bala_core::CoreError::CircularDependency {
            cycle: vec![a.id, hidden.id, a.id],
        };

        let message = super::dependency_error_message(&err, std::slice::from_ref(&a));

        assert_eq!(
            message,
            format!("would create a cycle: A → {} → A", id_text(hidden.id))
        );
    }

    #[test]
    fn dependency_error_message_should_keep_the_text_of_other_errors() {
        let err = bala_core::CoreError::EmptyTitle;

        let message = super::dependency_error_message(&err, &[]);

        assert_eq!(message, err.to_string());
    }
}
