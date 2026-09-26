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
use crate::tui::keymap;
use crate::tui::mode::{DetailField, EditableField, Mode, Pane};

/// Draws the current `App` state into `frame`.
///
/// When `app` is in `Mode::Help`, [`draw_help`] takes over the whole frame
/// area and nothing else is drawn this frame: a bordered overlay with no
/// alpha blending visually replaces whatever pane/mode was showing beneath
/// it anyway, so drawing that content first would be wasted work. Otherwise
/// dispatches on `app.pane()`: `Pane::List` renders a centered "No tasks
/// yet." message when there are no rows, otherwise a `List` of one line per
/// row with the selected row highlighted; `Pane::Detail` renders the
/// selected task's title and description via [`draw_detail`]. When `app` is
/// in `Mode::Insert`, an input line showing the in-progress buffer (and,
/// below it, any inline error) is drawn along the bottom of the frame,
/// regardless of pane.
pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();

    if let Mode::Help { previous } = app.mode() {
        draw_help(frame, previous, app.pane(), app.detail_field(), area);
        return;
    }

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

/// Indentation is capped at this many levels: beyond it, deeper rows all
/// render at the same (maximal) indent rather than growing further.
///
/// Without a cap, indenting a chain of `n` tasks allocates `"  ".repeat(0)`
/// through `"  ".repeat(n-1)` — quadratic total work in `n` (a 5,000-level
/// chain, which `render::task_rows` supports since hierarchy depth is unbounded,
/// would format roughly 25 million space characters on every single
/// redraw). A cap most users will never visually notice (a terminal rarely
/// shows more than this many columns of pure indentation usefully anyway)
/// bounds the per-row cost to a constant instead.
const MAX_INDENT_DEPTH: usize = 40;

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
            let glyph = if row.collapsed {
                '▸'
            } else if row.has_children {
                '▾'
            } else {
                ' '
            };
            let summary = row
                .direct_summary
                .map_or(String::new(), |(complete, total)| {
                    format!(" ({complete}/{total})")
                });
            let line = format!(
                "{}{glyph}[{marker}] {}{summary}  [{}]  {}",
                "  ".repeat(row.depth.min(MAX_INDENT_DEPTH)),
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
/// on the line below it, so a validation error shows up right under the
/// input that caused it. Used for both the List pane's new-task-title entry and the
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
        EditableField::NewSubtaskTitle(_) => "New subtask",
        EditableField::Parents(_) => "Parents (comma-separated ids)",
        EditableField::TypeKey(_) => "Type",
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
/// error line) since a confirm prompt carries its own key hint rather than
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

/// Renders the keybinding help overlay: one line per [`keymap::HelpEntry`]
/// for the mode/pane/field Help was opened from (`previous`, `pane`,
/// `detail_field` — passed straight to [`keymap::help_entries`]), each as
/// `"{key} — {description}"`, inside a bordered `Block` titled `"Help"`,
/// plus a fixed trailing `"Esc — close help"` line.
fn draw_help(
    frame: &mut Frame,
    previous: &Mode,
    pane: Pane,
    detail_field: DetailField,
    area: Rect,
) {
    let mut lines: Vec<String> = keymap::help_entries(previous, pane, detail_field)
        .iter()
        .map(|entry| format!("{} — {}", entry.key, entry.description))
        .collect();
    lines.push("Esc — close help".to_string());

    let paragraph = Paragraph::new(lines.join("\n")).block(Block::bordered().title("Help"));
    frame.render_widget(paragraph, area);
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

    use super::{MAX_INDENT_DEPTH, draw};
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
            depth: 0,
            has_children: false,
            collapsed: false,
            direct_summary: None,
            parent_id: None,
            grandparent_id: None,
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
    fn draw_should_indent_nested_row_under_its_parent() {
        let mut parent_row = row("Parent task", TaskStatus::Incomplete);
        parent_row.depth = 0;
        let mut child_row = row("Child task", TaskStatus::Incomplete);
        child_row.depth = 1;
        let app = App::new(vec![parent_row, child_row]);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let leading_spaces = |y: u16| -> usize {
            (0..buffer.area().width)
                .map(|x| buffer.cell((x, y)).unwrap().symbol())
                .take_while(|symbol| *symbol == " ")
                .count()
        };

        let parent_leading_spaces = leading_spaces(0);
        let child_leading_spaces = leading_spaces(1);
        assert_eq!(child_leading_spaces, parent_leading_spaces + 2);
    }

    #[test]
    fn draw_should_cap_indentation_at_max_indent_depth() {
        // Regression test for the indentation cap: without it, formatting a
        // row's indent string costs work proportional to its `depth`, so a
        // list with many deeply-nested rows would cost quadratic total work
        // per redraw. A row far beyond the cap must render with the SAME
        // indentation as a row exactly at the cap, not keep growing.
        let mut at_cap = row("At cap", TaskStatus::Incomplete);
        at_cap.depth = MAX_INDENT_DEPTH;
        let mut way_beyond_cap = row("Way beyond cap", TaskStatus::Incomplete);
        way_beyond_cap.depth = MAX_INDENT_DEPTH + 1000;
        let app = App::new(vec![at_cap, way_beyond_cap]);
        let backend = TestBackend::new(120, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let leading_spaces = |y: u16| -> usize {
            (0..buffer.area().width)
                .map(|x| buffer.cell((x, y)).unwrap().symbol())
                .take_while(|symbol| *symbol == " ")
                .count()
        };

        assert_eq!(leading_spaces(0), leading_spaces(1));
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

    #[test]
    fn draw_should_render_help_overlay_listing_normal_list_bindings_when_help_opened_from_normal_list()
     {
        let task_row = row("Write docs", TaskStatus::Incomplete);
        let mut app = App::new(vec![task_row]);
        let mut core = core();
        let _ = apply_action(&mut app, &mut core, Action::OpenHelp);

        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Help"));
        assert!(text.contains("j/↓ — Move the selection down"));
        assert!(text.contains("dd — Delete the selected task (press d twice)"));
    }

    #[test]
    fn draw_should_render_help_overlay_reflecting_insert_mode_bindings_when_help_opened_from_insert()
     {
        let mut app = App::new(vec![]);
        let mut core = core();
        let _ = apply_action(&mut app, &mut core, Action::StartInsertNewTitle);
        let _ = apply_action(&mut app, &mut core, Action::OpenHelp);

        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("F1 — Show the keybinding help overlay"));
        assert!(!text.contains("j/↓ — Move the selection down"));
    }

    #[test]
    fn draw_should_render_esc_close_hint_in_help_overlay() {
        let task_row = row("Write docs", TaskStatus::Incomplete);
        let mut app = App::new(vec![task_row]);
        let mut core = core();
        let _ = apply_action(&mut app, &mut core, Action::OpenHelp);

        let backend = TestBackend::new(60, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Esc — close help"));
    }

    #[test]
    fn draw_list_should_show_collapse_glyph_and_hide_children_when_collapsed() {
        let mut parent = row("Parent", TaskStatus::Incomplete);
        parent.has_children = true;
        parent.collapsed = true;
        parent.direct_summary = Some((0, 1));
        let app = App::new(vec![parent]);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains('▸'));
        assert!(!text.contains("Child"));
    }

    #[test]
    fn draw_list_should_show_expand_glyph_when_expanded_with_children() {
        let mut parent = row("Parent", TaskStatus::Incomplete);
        parent.has_children = true;
        parent.collapsed = false;
        let mut child = row("Child", TaskStatus::Incomplete);
        child.depth = 1;
        let app = App::new(vec![parent, child]);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains('▾'));
    }

    #[test]
    fn draw_list_should_show_no_glyph_for_leaf_task() {
        let leaf = row("Leaf task", TaskStatus::Incomplete);
        let app = App::new(vec![leaf]);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let text = buffer_text(terminal.backend().buffer());
        assert!(!text.contains('▸'));
        assert!(!text.contains('▾'));
        assert!(text.contains("[ ] Leaf task"));
    }

    #[test]
    fn draw_list_should_show_direct_child_summary_on_collapsed_parent() {
        let mut parent = row("Parent", TaskStatus::Incomplete);
        parent.has_children = true;
        parent.collapsed = true;
        parent.direct_summary = Some((1, 2));
        let app = App::new(vec![parent]);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("(1/2)"));
    }

    #[test]
    fn draw_list_should_still_dim_and_check_completed_row_when_collapsed() {
        let mut parent = row("Parent", TaskStatus::Complete);
        parent.has_children = true;
        parent.collapsed = true;
        parent.direct_summary = Some((1, 1));
        let app = App::new(vec![parent]);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("[x] Parent"));

        let buffer = terminal.backend().buffer();
        let completed_row_style = buffer.cell((0, 0)).unwrap().style();
        assert!(completed_row_style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn draw_should_render_help_overlay_over_detail_pane_when_help_opened_from_detail() {
        let task_row = row("Write docs", TaskStatus::Incomplete);
        let mut app = App::new(vec![task_row]);
        let mut core = core();
        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);
        let _ = apply_action(&mut app, &mut core, Action::OpenHelp);

        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Esc — Leave the Detail pane, back to the list"));
        assert!(text.contains("i — Edit title"));
    }
}
