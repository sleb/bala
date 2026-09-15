//! Renders `App`'s selection state to a ratatui frame.
//!
//! Kept separate from `app` so drawing logic can be exercised against
//! `ratatui::backend::TestBackend` (no real terminal) before the real
//! crossterm event loop wires it up.

use bala_core::TaskStatus;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};

use crate::tui::app::App;
use crate::tui::mode::{DetailField, EditableField, Mode, Pane};

/// Draws the current `App` state into `frame`.
///
/// Dispatches on `app.pane()`: `Pane::List` renders a centered "No tasks
/// yet." message when there are no rows, otherwise a `List` of one line per
/// row with the selected row highlighted; `Pane::Detail` renders the
/// selected task's title and description via [`draw_detail`]. When `app` is
/// in `Mode::Insert`, an input line showing the in-progress buffer (and,
/// below it, any inline error) is drawn along the bottom of the frame,
/// regardless of pane.
pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let (content_area, input_area) = split_for_input(app, area);

    match app.pane() {
        Pane::List => {
            if app.rows().is_empty() {
                draw_empty(frame, content_area);
            } else {
                draw_list(frame, app, content_area);
            }
        }
        Pane::Detail => draw_detail(frame, app, content_area),
    }

    if let Some(input_area) = input_area {
        draw_insert_input(frame, app, input_area);
    }

    if matches!(app.mode(), Mode::Confirm { .. }) {
        draw_confirm(frame, app, content_area);
    }
}

/// Splits `area` into a list area and, when `app` is in `Mode::Insert`, a
/// trailing input area (two rows: the buffer line and an error line).
fn split_for_input(app: &App, area: Rect) -> (Rect, Option<Rect>) {
    if matches!(app.mode(), Mode::Insert { .. }) {
        let [list_area, input_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(2)]).areas(area);
        (list_area, Some(input_area))
    } else {
        (area, None)
    }
}

/// Renders the task list with the selected row highlighted.
fn draw_list(frame: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .rows()
        .iter()
        .map(|row| {
            let marker = if row.status == TaskStatus::Complete {
                'x'
            } else {
                ' '
            };
            let line = format!(
                "[{marker}] {}  [{}]  {}",
                row.title,
                row.type_label,
                row.assignee_name.as_deref().unwrap_or("Unassigned")
            );
            let item = ListItem::new(line);
            if row.status == TaskStatus::Complete {
                item.style(Style::new().add_modifier(Modifier::DIM))
            } else {
                item
            }
        })
        .collect();

    let list = List::new(items).highlight_style(Style::new().add_modifier(Modifier::REVERSED));

    let mut state = ListState::default();
    state.select(app.selected_index());

    frame.render_stateful_widget(list, area, &mut state);
}

/// Renders the `Mode::Insert` buffer as an editable line, labeled by which
/// field is being edited, plus any inline error message from `app.error()`
/// on the line below it (AC5: validation errors are shown inline in the
/// entry field). Used for both the List pane's new-task-title entry and the
/// Detail pane's title/description edits — the same input+error layout,
/// just a different label depending on `EditableField`.
fn draw_insert_input(frame: &mut Frame, app: &App, area: Rect) {
    let Mode::Insert { field, buffer } = app.mode() else {
        return;
    };
    let label = match field {
        EditableField::NewTitle => "New task",
        EditableField::Title(_) => "Title",
        EditableField::Description(_) => "Description",
    };
    let [buffer_area, error_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);

    let input = Paragraph::new(format!("{label}: {buffer}_"));
    frame.render_widget(input, buffer_area);

    if let Some(error) = app.error() {
        let error_line = Paragraph::new(error).style(Style::new().fg(Color::Red));
        frame.render_widget(error_line, error_area);
    }
}

/// Renders the `Mode::Confirm` prompt as a single line along the bottom of
/// `area`, overlaid on whatever pane is showing beneath it — the same
/// "last line of the frame" convention `draw_insert_input` uses for the
/// new-task/edit entry line, but a single, self-contained line (no separate
/// error line) since a confirm prompt carries its own y/n hint rather than
/// a validation error.
fn draw_confirm(frame: &mut Frame, app: &App, area: Rect) {
    let Mode::Confirm { prompt, .. } = app.mode() else {
        return;
    };
    let [_, prompt_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);

    let line = Paragraph::new(prompt.as_str()).style(Style::new().fg(Color::Yellow));
    frame.render_widget(line, prompt_area);
}

/// Renders the "No tasks yet." message, centered within `area`.
fn draw_empty(frame: &mut Frame, area: Rect) {
    let paragraph = Paragraph::new("No tasks yet.")
        .alignment(Alignment::Center)
        .block(Block::default());
    frame.render_widget(paragraph, area);
}

/// Renders the Detail pane: the selected task's title and description, each
/// on its own line, with the line matching `app.detail_field()` highlighted
/// the same way the list highlights its selected row (`Modifier::REVERSED`).
///
/// Renders nothing (an empty area) when there's no selected row — shouldn't
/// normally happen, since the Detail pane can only be entered from a
/// non-empty list, but avoids a panic if it ever does.
fn draw_detail(frame: &mut Frame, app: &App, area: Rect) {
    let Some(selected) = app.selected_row() else {
        return;
    };
    let description = app.description_of(selected.id).unwrap_or("");

    let [title_area, description_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);

    let highlight = Style::new().add_modifier(Modifier::REVERSED);

    let title_style = if app.detail_field() == DetailField::Title {
        highlight
    } else {
        Style::default()
    };
    let title_line = Paragraph::new(format!("Title: {}", selected.title)).style(title_style);
    frame.render_widget(title_line, title_area);

    let description_style = if app.detail_field() == DetailField::Description {
        highlight
    } else {
        Style::default()
    };
    let description_line =
        Paragraph::new(format!("Description: {description}")).style(description_style);
    frame.render_widget(description_line, description_area);
}

#[cfg(test)]
mod tests {
    use bala_core::{TaskId, TaskStatus};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Modifier;

    use std::collections::HashMap;

    use bala_core::{Core, InMemoryStore};

    use super::draw;
    use crate::render::TaskRow;
    use crate::tui::app::{App, apply_action};
    use crate::tui::keymap::Action;

    fn row(title: &str, status: TaskStatus) -> TaskRow {
        TaskRow {
            id: TaskId::new(),
            title: title.to_string(),
            type_label: "task".to_string(),
            status,
            assignee_name: None,
        }
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    #[test]
    fn draw_should_render_completed_row_with_checkbox_marker_and_dim_style() {
        let rows = vec![
            row("First task", TaskStatus::Incomplete),
            row("Second task", TaskStatus::Complete),
        ];
        let app = App::new(rows);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("[ ] First task"));
        assert!(text.contains("[x] Second task"));

        let buffer = terminal.backend().buffer();
        let completed_row_style = buffer.cell((0, 1)).unwrap().style();
        assert!(completed_row_style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn draw_should_render_incomplete_row_without_dim_style() {
        let rows = vec![row("First task", TaskStatus::Incomplete)];
        let app = App::new(rows);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let incomplete_row_style = buffer.cell((0, 0)).unwrap().style();
        assert!(!incomplete_row_style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn draw_should_highlight_the_selected_row() {
        let rows = vec![
            row("First task", TaskStatus::Incomplete),
            row("Second task", TaskStatus::Incomplete),
        ];
        let app = App::new(rows);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let selected_style = buffer.cell((0, 0)).unwrap().style();
        let unselected_style = buffer.cell((0, 1)).unwrap().style();

        assert_ne!(selected_style, unselected_style);
        assert!(selected_style.add_modifier.contains(Modifier::REVERSED));
        assert!(!unselected_style.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn draw_should_render_no_tasks_yet_message_when_rows_empty() {
        let app = App::new(vec![]);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("No tasks yet."));
    }

    fn core() -> Core<InMemoryStore> {
        Core::new(InMemoryStore::default()).expect("in-memory core should construct")
    }

    #[test]
    fn draw_detail_should_render_selected_task_title_and_description() {
        let task_row = row("Write docs", TaskStatus::Incomplete);
        let id = task_row.id;
        let mut descriptions = HashMap::new();
        descriptions.insert(id, Some("Document the CLI commands".to_string()));
        let mut app = App::new(vec![task_row]).with_descriptions(descriptions);
        let mut core = core();
        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);

        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Write docs"));
        assert!(text.contains("Document the CLI commands"));
    }

    #[test]
    fn draw_detail_should_highlight_the_field_cursor() {
        let task_row = row("Write docs", TaskStatus::Incomplete);
        let mut app = App::new(vec![task_row]);
        let mut core = core();
        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);
        let _ = apply_action(&mut app, &mut core, Action::DetailCursorDown);

        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let title_style = buffer.cell((0, 0)).unwrap().style();
        let description_style = buffer.cell((0, 1)).unwrap().style();

        assert!(!title_style.add_modifier.contains(Modifier::REVERSED));
        assert!(description_style.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn draw_detail_should_show_inline_error_message_when_present() {
        let task_row = row("Write docs", TaskStatus::Incomplete);
        let mut app = App::new(vec![task_row]);
        let mut core = core();
        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);
        let _ = apply_action(&mut app, &mut core, Action::StartEditTitle);
        for _ in 0.."Write docs".chars().count() {
            let _ = apply_action(&mut app, &mut core, Action::Backspace);
        }
        let _ = apply_action(&mut app, &mut core, Action::SubmitInsert);

        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let text = buffer_text(terminal.backend().buffer());
        assert!(app.error().is_some());
        assert!(text.contains(app.error().unwrap()));
    }

    #[test]
    fn draw_confirm_should_render_prompt_text_naming_the_task_and_yn_hint() {
        let task_row = row("Write docs", TaskStatus::Incomplete);
        let mut app = App::new(vec![task_row]);
        let mut core = core();
        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);
        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);

        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Write docs"));
        assert!(text.contains("y/n"));
    }
}
