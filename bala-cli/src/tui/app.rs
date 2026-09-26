//! Selection-state and edit-mode model for the TUI's task list view.
//!
//! `App` tracks the current row selection over an in-memory list of
//! `TaskRow`s, plus the current interaction `Mode` and any inline error to
//! show the user. `apply_action` is the single place that mutates `App` and
//! (for actions that need it) calls through to `Core`; `handle_key` is a
//! thin wrapper around `keymap::key_to_action` + `apply_action` for the
//! real crossterm-backed event loop in `tui::mod::run`.

use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;

use bala_core::{
    Core, CoreError, DeleteMode, Field, NewTask, SiblingOrder, Store, Task, TaskId, TaskPatch,
    TaskStatus, UserId,
};
use crossterm::event::KeyEvent;

use crate::render::{self, TaskRow};
use crate::tui::keymap::{Action, key_to_action};
use crate::tui::mode::{DetailField, EditableField, Mode, Pane, PendingAction};

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
            sibling_order: SiblingOrder::default(),
            collapsed: HashSet::new(),
            pending_d: false,
            type_filter: None,
            available_type_keys: Vec::new(),
        }
    }

    /// Attaches the cached full task fetch used by [`App::rebuild_rows`] to
    /// re-derive `rows` after a collapse/expand action, without needing to
    /// re-fetch from `Core`. Builder-style for the same reason as
    /// [`App::with_lookup_maps`].
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
    /// going forward.
    #[must_use]
    pub fn with_type_filter_state(
        mut self,
        type_filter: Option<String>,
        available_type_keys: Vec<String>,
    ) -> Self {
        self.type_filter = type_filter;
        self.available_type_keys = available_type_keys;
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

    /// Re-derives `rows` from `tasks`/`type_labels`/`user_names`/`collapsed`,
    /// reselecting the row that was focused before the rebuild so a
    /// collapse/expand never loses the user's place. `CollapseFocused`/
    /// `ExpandFocused` only ever act on the focused row itself, so its id
    /// always survives those two rebuilds. `CollapseAll`, however, can
    /// collapse an *ancestor* of the focused row too, hiding the focused
    /// row itself — in that case `select_id_or_nearest_visible_ancestor`
    /// walks up `parent_ids` to reselect the nearest still-visible ancestor
    /// instead of leaving `self.selected` as a stale index into the old
    /// (differently-shaped) `rows`.
    fn rebuild_rows(&mut self) {
        let selected_id = self.selected_row().map(|row| row.id);
        self.rows = render::task_rows(
            &self.tasks,
            &self.sibling_order,
            &self.type_labels,
            &self.user_names,
            &self.collapsed,
        );
        if let Some(id) = selected_id {
            self.select_id_or_nearest_visible_ancestor(id);
        }
    }

    /// Selects the row for `id` if it's present in `self.rows`; otherwise
    /// walks up `id`'s `parent_ids` (in `self.tasks`) looking for the
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
            let Some(&parent_id) = task.parent_ids.first() else {
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
/// `EditableField::Parents`, prefilled with the selected task's current
/// parent ids (fetched fresh via `Core::get_task`), and `SubmitInsert` for
/// that field calls `Core::set_parents`. `StartSetType` enters `Mode::Insert`
/// scoped to `EditableField::TypeKey`, prefilled with the selected task's
/// current `type_key` (fetched fresh via `Core::get_task`), and
/// `SubmitInsert` for that field calls `Core::update_task` with a
/// `TaskPatch { type_key: Field::Set(...), .. }`, surfacing
/// `CoreError::UnknownTaskType` inline like other validation errors when the
/// typed key names no configured type. `OpenHelp` enters `Mode::Help`,
/// remembering the current mode as `previous`; `CloseHelp` restores
/// `previous` if `app` is currently in `Mode::Help`, otherwise it does
/// nothing. `CycleTypeFilter` advances `app.type_filter` through `None ->
/// available_type_keys[0] -> ... -> None` (see `cycle_type_filter`) and then
/// calls `refresh_rows_from_tree` so the visible rows immediately reflect
/// the new filter. `Noop` does nothing.
#[allow(clippy::too_many_lines)] // one big dispatch table by design; see doc comment above
pub fn apply_action<S: Store>(
    app: &mut App,
    core: &mut Core<S>,
    action: Action,
) -> ControlFlow<()> {
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
            handle_d_key_pressed(app, was_pending_d);
            ControlFlow::Continue(())
        }
        Action::ConfirmYes => {
            confirm_yes(app, core);
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
/// its neighbor among the siblings of the parent it is *rendered under*
/// (`row.parent_id`), via `Core::move_sibling`. The swap uses the full
/// sibling order, so with a type filter active the neighbor may be hidden
/// and a press can look like a no-op. On success the tree is refetched and
/// selection stays on the moved task under the same parent (a task shown
/// under several parents may have several rows). At either end of the
/// sibling list nothing changes and no error is shown; Core errors are
/// surfaced via `app.error`.
fn move_focused_task<S: Store>(app: &mut App, core: &mut Core<S>, direction: bala_core::Direction) {
    let Some(row) = app.selected_row() else {
        return;
    };
    let (id, parent) = (row.id, row.parent_id);
    match core.move_sibling(parent, id, direction) {
        Ok(false) => {}
        Ok(true) => {
            app.error = None;
            refresh_rows_from_tree(app, core, Some(id));
            if let Some(pos) = app
                .rows
                .iter()
                .position(|r| r.id == id && r.parent_id == parent)
            {
                app.selected = Some(pos);
            }
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
/// `Core::outdent_task`, using the focused row's rendered path
/// (`parent_id`, `grandparent_id`). On indent the new parent (the previous
/// live sibling, read from the pre-move sibling order) is expanded so the
/// moved task stays visible. Selection follows the task to its row under
/// the new parent. `Ok(false)` is a silent no-op; Core errors (e.g.
/// circular hierarchy) are surfaced via `app.error`.
fn reparent_focused_task<S: Store>(app: &mut App, core: &mut Core<S>, kind: Reparent) {
    let Some(row) = app.selected_row() else {
        return;
    };
    let (id, parent, grandparent) = (row.id, row.parent_id, row.grandparent_id);
    let (result, new_parent) = match kind {
        Reparent::Indent => {
            // Re-read the order rather than trusting `app.sibling_order`,
            // which paths like delete leave stale, so the row expanded and
            // selected is the parent Core will actually pick.
            if let Ok(order) = core.sibling_order() {
                app.sibling_order = order;
            }
            let siblings = app.sibling_order.children_of(parent);
            let target = siblings
                .iter()
                .position(|&s| s == id)
                .and_then(|i| i.checked_sub(1))
                .map(|i| siblings[i]);
            (core.indent_task(parent, id), target)
        }
        Reparent::Outdent => (core.outdent_task(parent, grandparent, id), grandparent),
    };
    match result {
        Ok(false) => {}
        Ok(true) => {
            app.error = None;
            if matches!(kind, Reparent::Indent)
                && let Some(target) = new_parent
            {
                app.collapsed.remove(&target);
            }
            refresh_rows_from_tree(app, core, Some(id));
            if let Some(pos) = app
                .rows
                .iter()
                .position(|r| r.id == id && r.parent_id == new_parent)
            {
                app.selected = Some(pos);
            }
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
/// task's `parent_ids`), then rebuilds `app.rows`.
fn collapse_all(app: &mut App) {
    let parents_with_children: HashSet<TaskId> = app
        .tasks
        .iter()
        .flat_map(|task| task.parent_ids.iter().copied())
        .collect();
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
/// `Complete` task, calls `Core::reopen_task(id)` and refreshes that one
/// row on success (mirroring the same error-handling shape).
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
            }
            Err(err) => {
                app.error = Some(err.to_string());
            }
        },
    }
}

/// Updates `app.rows`' `status` field for every task in `touched`, matched
/// by id. Shared by [`handle_toggle_complete`] and [`confirm_yes`]'s
/// `CompleteCascade` arm so both "refresh every row a `Core` call actually
/// touched" call sites use the same lookup-by-id loop.
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
/// selected task, prefilled with its current parent ids as a
/// comma-separated list of UUIDs (empty string when it's already
/// top-level). No-op when there's no selection, or when a fresh
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
        .parent_ids
        .iter()
        .map(|parent_id| uuid::Uuid::from(*parent_id).to_string())
        .collect::<Vec<_>>()
        .join(",");

    app.mode = Mode::Insert {
        field: EditableField::Parents(id),
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
/// it for this action). On the second of two consecutive presses, enters
/// `Mode::Confirm` naming the selected task when there is one; with no
/// selection it's a no-op. On the first press, sets `app.pending_d` so the
/// next `DKeyPressed` is recognized as the second.
fn handle_d_key_pressed(app: &mut App, was_pending_d: bool) {
    if was_pending_d {
        if let Some(row) = app.selected_row() {
            let id = row.id;
            let title = row.title.clone();
            app.mode = Mode::Confirm {
                prompt: format!("Delete \"{title}\"? (y/n)"),
                action: PendingAction::Delete(id),
            };
        }
    } else {
        app.pending_d = true;
    }
}

/// Handles `Action::ConfirmYes`: runs the `Mode::Confirm`'s `PendingAction`.
///
/// For `PendingAction::Delete(id)`, calls `Core::delete_task` with
/// `DeleteMode::Subtree`: the TUI has no `PromoteChildren` choice yet, so
/// deleting a parent here takes with it every child that has no other live
/// parent (a child shared with another parent just loses its edge to the
/// deleted one and survives; for sole-parent children, the CLI's
/// `--promote-children` is the only way to keep them). On success, `app.rows`
/// is rebuilt from a fresh tree via `refresh_rows_from_tree` rather than
/// patched: in a multi-parent tree a shared child is rendered under several
/// parents and a surviving parent's `has_children` flag and rolled-up
/// progress can change, so only a full re-render gets every row right.
/// `app.error` is cleared before that refresh, so an error the refresh
/// itself sets (e.g. the follow-up `get_tree` failing after the delete
/// committed) is preserved. The selection keeps its previous index, clamped
/// to the new rows, and `app` returns to `Mode::Normal`/`Pane::List` (the
/// deleted task's Detail view no longer makes sense). On failure
/// `app.error` is set and `app` still returns to `Mode::Normal` — there's
/// no in-progress input to preserve here, unlike `submit_insert`'s failure
/// path.
///
/// For `PendingAction::CompleteCascade(id)`, calls
/// `Core::complete_task(id, true)`. On success every touched row's status is
/// refreshed (same lookup-by-id loop as [`handle_toggle_complete`]'s
/// non-cascade path) and `app` returns to `Mode::Normal`, leaving `app.pane`
/// untouched — unlike `Delete`, completing a task doesn't invalidate its
/// Detail view. On failure `app.error` is set and `app` returns to
/// `Mode::Normal`.
fn confirm_yes<S: Store>(app: &mut App, core: &mut Core<S>) {
    let Mode::Confirm { action, .. } = &app.mode else {
        return;
    };

    match *action {
        PendingAction::Delete(id) => match core.delete_task(id, DeleteMode::Subtree) {
            Ok(_) => {
                app.error = None;
                let previous = app.selected;
                refresh_rows_from_tree(app, core, None);
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
        },
        PendingAction::CompleteCascade(id) => match core.complete_task(id, true) {
            Ok(touched) => {
                refresh_row_statuses(app, &touched);
                app.mode = Mode::Normal;
                app.error = None;
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
        EditableField::Parents(id) => submit_reparent(app, core, id, &buffer.clone()),
        EditableField::TypeKey(id) => submit_set_type(app, core, id, &buffer.clone()),
    }
}

/// `EditableField::Parents(id)`: parses `buffer` as a comma-separated list
/// of UUIDs (an empty/whitespace-only buffer means "no parents", i.e.
/// promote `id` to top-level) and calls `Core::set_parents`. On success,
/// refreshes `app.rows` from the full tree via `refresh_rows_from_tree` so
/// `id` reappears at its new nested (or top-level) position, and returns to
/// `Mode::Normal`. On a parse failure (an entry that isn't a valid UUID) or
/// a `CoreError` from `set_parents` (e.g. `CircularHierarchy`), sets
/// `app.error` and leaves `app.mode` untouched so the buffer survives for
/// correction — matching `submit_new_title`'s/`submit_new_subtask`'s
/// existing "failure leaves mode untouched" convention. Any error
/// `refresh_rows_from_tree` itself sets on the success path is preserved,
/// not immediately cleared — the reparent already committed, so the user
/// still needs to see that the displayed rows may now be stale.
fn submit_reparent<S: Store>(app: &mut App, core: &mut Core<S>, id: TaskId, buffer: &str) {
    let trimmed = buffer.trim();
    let parse_result: Result<Vec<TaskId>, uuid::Error> = if trimmed.is_empty() {
        Ok(Vec::new())
    } else {
        trimmed
            .split(',')
            .map(|entry| entry.trim().parse::<uuid::Uuid>().map(TaskId::from))
            .collect()
    };

    let Ok(new_parents) = parse_result else {
        app.error = Some("invalid task id in parent list".to_string());
        return;
    };

    match core.set_parents(id, new_parents) {
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
/// `app.type_filter` (`None` filters nothing), caches it as `app.tasks`,
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
            app.sibling_order = sibling_order;
            app.rows = render::task_rows(
                &app.tasks,
                &app.sibling_order,
                &app.type_labels,
                &app.user_names,
                &app.collapsed,
            );
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
/// with `parent_ids: vec![parent_id]`. On success, refreshes `app.rows` from
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
        parent_ids: vec![parent_id],
        type_key: None,
        start_date: None,
        due_date: None,
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
        parent_ids: Vec::new(),
        type_key: None,
        start_date: None,
        due_date: None,
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
/// the new title. On success refreshes the matching row's displayed title;
/// on failure (e.g. an empty title) sets `app.error`.
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

    use bala_core::{Core, InMemoryStore, TaskId, TaskStatus, TaskType};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{App, apply_action, handle_key};
    use crate::render::TaskRow;
    use crate::tui::keymap::Action;
    use crate::tui::mode::{DetailField, EditableField, Mode, Pane};

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
            grandparent_id: None,
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
        assert!(app.rows().is_empty());
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
        assert!(app.rows().is_empty());
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
                parent_ids: Vec::new(),
                type_key: None,
                start_date: None,
                due_date: None,
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
                parent_ids: Vec::new(),
                type_key: None,
                start_date: None,
                due_date: None,
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
                parent_ids: Vec::new(),
                type_key: None,
                start_date: None,
                due_date: None,
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
                parent_ids: Vec::new(),
                type_key: None,
                start_date: None,
                due_date: None,
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
                parent_ids: Vec::new(),
                type_key: None,
                start_date: None,
                due_date: None,
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
        assert!(app.rows().is_empty());
        let tasks = core
            .get_tree(bala_core::TreeFilter::default())
            .expect("get_tree should succeed");
        assert!(!tasks.iter().any(|task| task.id == id));
    }

    #[test]
    fn apply_action_confirm_yes_delete_should_keep_row_of_child_with_another_parent() {
        let mut core = core();
        let a = core.create_task(minimal_new_task("A")).expect("create");
        let b = core.create_task(minimal_new_task("B")).expect("create");
        let c = child_of(&mut core, "C", a.id);
        core.set_parents(c.id, vec![a.id, b.id])
            .expect("set_parents should succeed");
        let mut app = app_from_core(&mut core);
        app.select_by_id(Some(a.id));
        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);
        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);

        let _ = apply_action(&mut app, &mut core, Action::ConfirmYes);

        assert_eq!(app.error(), None);
        assert_eq!(titles(&app), vec!["B", "C"]);
        let c_row = &app.rows()[1];
        assert_eq!(c_row.id, c.id);
        assert_eq!(c_row.depth, 1);
        assert_eq!(c_row.parent_id, Some(b.id));
        let selected = app.selected.expect("a row should stay selected");
        assert!(selected < app.rows().len());
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

    #[test]
    fn apply_action_confirm_no_should_return_to_normal_without_calling_delete_task() {
        let mut core = core();
        let task = core
            .create_task(bala_core::NewTask {
                title: "Write docs".to_string(),
                description: None,
                parent_ids: Vec::new(),
                type_key: None,
                start_date: None,
                due_date: None,
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

    #[test]
    fn move_key_should_act_on_the_path_the_row_is_rendered_under() {
        let mut core = core();
        let p1 = core.create_task(minimal_new_task("P1")).expect("create");
        let p2 = core.create_task(minimal_new_task("P2")).expect("create");
        let x = core
            .create_task(bala_core::NewTask {
                parent_ids: vec![p1.id, p2.id],
                ..minimal_new_task("X")
            })
            .expect("create");
        let _y1 = core
            .create_task(bala_core::NewTask {
                parent_ids: vec![p1.id],
                ..minimal_new_task("Y1")
            })
            .expect("create");
        let _y2 = core
            .create_task(bala_core::NewTask {
                parent_ids: vec![p2.id],
                ..minimal_new_task("Y2")
            })
            .expect("create");
        let mut app = app_from_core(&mut core);
        // Select X under P2 (the second rendered X row).
        let idx = app
            .rows()
            .iter()
            .position(|r| r.id == x.id && r.parent_id == Some(p2.id))
            .expect("X under P2");
        app.selected = Some(idx);

        let _ = handle_key(&mut app, &mut core, shift_key('J'));

        assert_eq!(titles(&app), vec!["P1", "X", "Y1", "P2", "Y2", "X"]);
        let sel = app.selected_row().expect("selection");
        assert_eq!((sel.id, sel.parent_id), (x.id, Some(p2.id)));
    }

    /// Selects the row for `id` rendered under `parent`.
    fn select_under(app: &mut App, id: TaskId, parent: Option<TaskId>) {
        let idx = app
            .rows()
            .iter()
            .position(|r| r.id == id && r.parent_id == parent)
            .expect("row under parent");
        app.selected = Some(idx);
    }

    fn child_of(core: &mut Core<InMemoryStore>, title: &str, parent: TaskId) -> bala_core::Task {
        core.create_task(bala_core::NewTask {
            parent_ids: vec![parent],
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

    #[test]
    fn indent_key_should_show_inline_error_on_circular_hierarchy() {
        let mut core = core();
        let p = core.create_task(minimal_new_task("P")).expect("create");
        let y2 = child_of(&mut core, "Y2", p.id);
        let x2 = child_of(&mut core, "X2", p.id);
        // Y2 (previous sibling of X2) is also a child of X2.
        core.set_parents(y2.id, vec![p.id, x2.id])
            .expect("set_parents");
        let mut app = app_from_core(&mut core);
        select_under(&mut app, x2.id, Some(p.id));

        let _ = handle_key(&mut app, &mut core, shift_key('L'));

        assert!(app.error().is_some());
        assert_eq!(
            app.selected_row().map(|r| (r.id, r.parent_id)),
            Some((x2.id, Some(p.id)))
        );
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
            grandparent_id: None,
        }
    }

    fn minimal_new_task(title: &str) -> bala_core::NewTask {
        bala_core::NewTask {
            title: title.to_string(),
            description: None,
            parent_ids: Vec::new(),
            type_key: None,
            start_date: None,
            due_date: None,
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
                parent_ids: vec![parent.id],
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
                parent_ids: vec![parent.id],
                ..minimal_new_task("Child")
            })
            .expect("create_task should succeed");
        let _grandchild = core
            .create_task(bala_core::NewTask {
                parent_ids: vec![child.id],
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
                parent_ids: vec![parent.id],
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
                parent_ids: vec![parent.id],
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
        assert!(app.rows().is_empty());
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
        assert_eq!(child.parent_ids, vec![parent.id]);
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
    fn apply_action_start_reparent_should_prefill_buffer_with_current_parent_ids() {
        let mut core = core();
        let parent = core
            .create_task(minimal_new_task("Parent"))
            .expect("create_task should succeed");
        let child = core
            .create_task(bala_core::NewTask {
                parent_ids: vec![parent.id],
                ..minimal_new_task("Child")
            })
            .expect("create_task should succeed");
        let mut app = App::new(vec![row_for(&child)]);

        let _ = apply_action(&mut app, &mut core, Action::StartReparent);

        match app.mode() {
            Mode::Insert {
                field: EditableField::Parents(id),
                buffer,
            } => {
                assert_eq!(*id, child.id);
                assert_eq!(*buffer, uuid::Uuid::from(parent.id).to_string());
            }
            other => panic!("expected Mode::Insert with Parents field, got {other:?}"),
        }
    }

    #[test]
    fn apply_action_submit_reparent_should_call_set_parents_and_move_task_in_tree() {
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
    fn apply_action_submit_reparent_with_empty_buffer_should_promote_to_top_level() {
        let mut core = core();
        let parent = core
            .create_task(minimal_new_task("Parent"))
            .expect("create_task should succeed");
        let child = core
            .create_task(bala_core::NewTask {
                parent_ids: vec![parent.id],
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
                field: EditableField::Parents(id),
                buffer,
            } => {
                assert_eq!(*id, child.id);
                assert!(!buffer.is_empty());
                buffer.chars().count()
            }
            other => panic!("expected Mode::Insert with Parents field, got {other:?}"),
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
        assert!(updated_child.parent_ids.is_empty());
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
                field: EditableField::Parents(id),
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
                field: EditableField::Parents(id),
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
                parent_ids: vec![parent.id],
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
                parent_ids: vec![parent.id],
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
                parent_ids: vec![parent_a.id],
                ..minimal_new_task("Child A")
            })
            .expect("create_task should succeed");
        let parent_b = core
            .create_task(minimal_new_task("Parent B"))
            .expect("create_task should succeed");
        let _child_b = core
            .create_task(bala_core::NewTask {
                parent_ids: vec![parent_b.id],
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
                parent_ids: vec![parent_a.id],
                ..minimal_new_task("Child A")
            })
            .expect("create_task should succeed");
        let parent_b = core
            .create_task(minimal_new_task("Parent B"))
            .expect("create_task should succeed");
        let _child_b = core
            .create_task(bala_core::NewTask {
                parent_ids: vec![parent_b.id],
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
                parent_ids: vec![parent.id],
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
                parent_ids: vec![parent.id],
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
        // Regression: `confirm_yes`'s `Delete` arm used to filter only
        // `app.rows`, leaving the deleted task(s) in `app.tasks`. A later
        // collapse/expand action would then re-derive `rows` from the stale
        // `app.tasks` and resurrect the deleted row(s).
        let mut core = core();
        let parent = core
            .create_task(minimal_new_task("Parent"))
            .expect("create_task should succeed");
        let child = core
            .create_task(bala_core::NewTask {
                parent_ids: vec![parent.id],
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
                parent_ids: vec![parent.id],
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
                parent_ids: vec![root.id],
                ..minimal_new_task("Parent")
            })
            .expect("create_task should succeed");
        let child = core
            .create_task(bala_core::NewTask {
                parent_ids: vec![parent.id],
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

        assert!(app.rows().is_empty());
        assert_eq!(app.selected_index(), None);
    }
}
