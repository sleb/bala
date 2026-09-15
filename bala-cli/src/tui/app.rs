//! Selection-state and edit-mode model for the TUI's task list view.
//!
//! `App` tracks the current row selection over an in-memory list of
//! `TaskRow`s, plus the current interaction `Mode` and any inline error to
//! show the user. `apply_action` is the single place that mutates `App` and
//! (for actions that need it) calls through to `Core`; `handle_key` is a
//! thin wrapper around `keymap::key_to_action` + `apply_action` for the
//! real crossterm-backed event loop in `tui::mod::run`.

use std::collections::HashMap;
use std::ops::ControlFlow;

use bala_core::{
    Core, CoreError, DeleteMode, Field, NewTask, Store, TaskId, TaskPatch, TaskStatus, UserId,
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
    /// Whether a single `d` key press is pending a second consecutive `d` to
    /// complete the `dd` delete-confirm sequence. Reset to `false` by every
    /// action other than `DKeyPressed` itself, so `d`, some unrelated
    /// action, `d` doesn't count as two consecutive presses.
    pending_d: bool,
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
            pending_d: false,
        }
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
    /// never needed one before this checkpoint). Builder-style for the same
    /// reason as [`App::with_lookup_maps`].
    #[must_use]
    pub fn with_descriptions(mut self, descriptions: HashMap<TaskId, Option<String>>) -> Self {
        self.descriptions = descriptions;
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
/// description). `OpenHelp` enters `Mode::Help`, remembering the current mode
/// as `previous`; `CloseHelp` restores `previous` if `app` is currently in
/// `Mode::Help`, otherwise it does nothing. `Noop` does nothing.
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
            app.mode = Mode::Insert {
                field: EditableField::NewTitle,
                buffer: String::new(),
            };
            app.error = None;
            ControlFlow::Continue(())
        }
        Action::InsertChar(c) => {
            if let Mode::Insert { buffer, .. } = &mut app.mode {
                buffer.push(c);
            }
            ControlFlow::Continue(())
        }
        Action::Backspace => {
            if let Mode::Insert { buffer, .. } = &mut app.mode {
                buffer.pop();
            }
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
            if app.selected_row().is_some() {
                app.pane = Pane::Detail;
                app.detail_field = DetailField::Title;
                app.error = None;
            }
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
        Action::Noop => ControlFlow::Continue(()),
    }
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
fn refresh_row_statuses(app: &mut App, touched: &[bala_core::Task]) {
    for task in touched {
        if let Some(row) = app.rows.iter_mut().find(|row| row.id == task.id) {
            row.status = task.status;
        }
    }
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
/// `DeleteMode::Subtree` (no TUI-created task can have children yet, so
/// `PromoteChildren`'s choice UI is out of scope until Epic 3's hierarchy
/// lands in the TUI). On success, every task actually touched (the deleted
/// task and any descendants also tombstoned) is removed from `app.rows`,
/// the selection is clamped to the remaining rows, and `app` returns to
/// `Mode::Normal`/`Pane::List` (the deleted task's Detail view no longer
/// makes sense). On failure `app.error` is set and `app` still returns to
/// `Mode::Normal` — there's no in-progress input to preserve here, unlike
/// `submit_insert`'s failure path.
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
            Ok(deleted) => {
                let deleted_ids: std::collections::HashSet<TaskId> =
                    deleted.iter().map(|task| task.id).collect();
                app.rows.retain(|row| !deleted_ids.contains(&row.id));
                app.selected = if app.rows.is_empty() {
                    None
                } else {
                    Some(app.selected.unwrap_or(0).min(app.rows.len() - 1))
                };
                app.pane = Pane::List;
                app.mode = Mode::Normal;
                app.error = None;
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
    }
}

/// `EditableField::NewTitle`: calls `Core::create_task` with `buffer` as the
/// title. On success appends+selects the new row; on failure (e.g. an empty
/// title) sets `app.error`.
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
            let new_rows = render::task_rows(
                std::slice::from_ref(&task),
                &app.type_labels,
                &app.user_names,
            );
            app.rows.extend(new_rows);
            app.selected = Some(app.rows.len() - 1);
            app.mode = Mode::Normal;
            app.error = None;
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
            if let Some(updated) = tasks.iter().find(|task| task.id == id)
                && let Some(row) = app.rows.iter_mut().find(|row| row.id == id)
            {
                row.title.clone_from(&updated.title);
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
    use std::ops::ControlFlow;

    use bala_core::{Core, InMemoryStore, TaskId, TaskStatus};
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

    fn row_for(task: &bala_core::Task) -> TaskRow {
        TaskRow {
            id: task.id,
            title: task.title.clone(),
            type_label: "task".to_string(),
            status: task.status,
            assignee_name: None,
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
    fn select_by_id_with_none_should_be_noop() {
        let row_a = row("First");
        let row_b = row("Second");
        let mut app = App::new(vec![row_a.clone(), row_b]);

        app.select_by_id(None);

        assert_eq!(app.selected_row(), Some(&row_a));
    }
}
