//! Pure selection-state model for the TUI's task list view.
//!
//! `App` tracks the current row selection over an in-memory list of
//! `TaskRow`s. It has no dependency on ratatui or crossterm: later
//! checkpoints wire real key events to `move_up`/`move_down` and render the
//! selection with `screens`, but this module is unit-testable on its own.

use std::ops::ControlFlow;

use crossterm::event::{KeyCode, KeyEvent};

use crate::render::TaskRow;

/// Selection state over an in-memory list of task rows.
///
/// Selection-boundary behavior is clamp, not wrap: `move_up` at the first
/// row and `move_down` at the last row both leave the selection unchanged.
pub struct App {
    rows: Vec<TaskRow>,
    selected: Option<usize>,
}

impl App {
    /// Builds a new `App` over `rows`, selecting the first row when present.
    #[must_use]
    pub fn new(rows: Vec<TaskRow>) -> Self {
        let selected = if rows.is_empty() { None } else { Some(0) };
        Self { rows, selected }
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
    ///
    /// Not yet called by any production code path — `screens::draw` only
    /// needs `rows()`/`selected_index()` to render the list. This story's
    /// checkpoint plan specifies it anyway, as the accessor a later story's
    /// detail view (opened via `Enter` on the focused task, per LLD-3's
    /// Normal-mode keymap) will need. Kept as real, tested public API rather
    /// than deleted, so a future story doesn't have to re-derive it.
    #[must_use]
    #[allow(
        dead_code,
        reason = "plan-specified API for a later story's detail view; only tests call it today"
    )]
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
}

/// Dispatches one key event against `app`.
///
/// There is only one interaction mode today (see [`crate::tui::mode::Mode`]),
/// so this doesn't yet take a `Mode` parameter: every key means the same
/// thing regardless of mode, because there's only one. `j`/`Down` and
/// `k`/`Up` move the selection; `q` signals quit via `ControlFlow::Break`;
/// anything else is a no-op.
pub fn handle_key(app: &mut App, key: KeyEvent) -> ControlFlow<()> {
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => {
            app.move_down();
            ControlFlow::Continue(())
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.move_up();
            ControlFlow::Continue(())
        }
        KeyCode::Char('q') => ControlFlow::Break(()),
        _ => ControlFlow::Continue(()),
    }
}

#[cfg(test)]
mod tests {
    use std::ops::ControlFlow;

    use bala_core::{TaskId, TaskStatus};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::render::TaskRow;
    use crate::tui::app::{App, handle_key};

    fn row(title: &str) -> TaskRow {
        TaskRow {
            id: TaskId::new(),
            title: title.to_string(),
            type_label: "task".to_string(),
            status: TaskStatus::Incomplete,
            assignee_name: None,
        }
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

        let result = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        );
        assert_eq!(result, ControlFlow::Continue(()));
        assert_eq!(app.selected_index(), Some(1));

        let result = handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(result, ControlFlow::Continue(()));
        assert_eq!(app.selected_index(), Some(2));
    }

    #[test]
    fn handle_key_k_and_up_arrow_should_move_selection_up() {
        let mut app = App::new(vec![row("First"), row("Second"), row("Third")]);
        app.move_down();
        app.move_down();
        assert_eq!(app.selected_index(), Some(2));

        let result = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
        );
        assert_eq!(result, ControlFlow::Continue(()));
        assert_eq!(app.selected_index(), Some(1));

        let result = handle_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(result, ControlFlow::Continue(()));
        assert_eq!(app.selected_index(), Some(0));
    }

    #[test]
    fn handle_key_q_should_signal_quit() {
        let mut app = App::new(vec![row("First"), row("Second")]);

        let result = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );

        assert_eq!(result, ControlFlow::Break(()));
        // Quitting doesn't mutate selection.
        assert_eq!(app.selected_index(), Some(0));
    }

    #[test]
    fn handle_key_unmapped_key_should_be_noop_and_continue() {
        let mut app = App::new(vec![row("First"), row("Second")]);

        let result = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE),
        );

        assert_eq!(result, ControlFlow::Continue(()));
        assert_eq!(app.selected_index(), Some(0));
    }
}
