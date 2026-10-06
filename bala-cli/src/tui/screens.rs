//! Renders `App`'s selection state to a ratatui frame.
//!
//! Kept separate from `app` so drawing logic can be exercised against
//! `ratatui::backend::TestBackend` (no real terminal) before the real
//! crossterm event loop wires it up.

use bala_core::TaskStatus;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};

use crate::tui::app::{App, BlockedView, RelatedTask};
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
/// selected task's title, description and dependency lists via
/// [`draw_detail`]. When `app` is
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
    let content_area = if app.pane() == Pane::List && app.blocked_view() != BlockedView::All {
        let [list_area, status_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(content_area);
        draw_status_line(frame, app, status_area);
        list_area
    } else {
        content_area
    };

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

/// Renders the one-row status line naming the active blocked/ready view.
fn draw_status_line(frame: &mut Frame, app: &App, area: Rect) {
    let text = format!("view: {}", app.blocked_view().label());
    frame.render_widget(
        Paragraph::new(text).style(Style::new().add_modifier(Modifier::BOLD)),
        area,
    );
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

/// Marker shown after the checkbox of a task waiting on an incomplete
/// predecessor.
const BLOCKED_GLYPH: &str = "⊘";

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
            let head = format!(
                "{}{glyph}[{marker}] ",
                "  ".repeat(row.depth.min(MAX_INDENT_DEPTH))
            );
            let tail = format!(
                "{}{summary}  [{}]  {}",
                row.title,
                row.type_label,
                row.assignee_name.as_deref().unwrap_or("Unassigned")
            );
            let mut spans = vec![Span::raw(head)];
            if row.blocked {
                spans.push(Span::styled(
                    format!("{BLOCKED_GLYPH} "),
                    Style::new().fg(Color::Red),
                ));
            }
            spans.push(Span::raw(tail));
            let item = ListItem::new(Line::from(spans));
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
        EditableField::Parent(_) => "Parent id",
        EditableField::TypeKey(_) => "Type",
        EditableField::AddPredecessor(_) => "Depends on (task id)",
        EditableField::RemovePredecessor(_) => "Remove dependency on (task id)",
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

/// Renders the `Mode::Confirm` prompt along the bottom of `area`, overlaid
/// on whatever pane is showing beneath it — the same "bottom of the frame"
/// convention `draw_insert_input` uses for the new-task/edit entry line,
/// but self-contained (no separate error line) since a confirm prompt
/// carries its own key hint rather than a validation error.
///
/// The prompt takes one row per line of its text, so its last line — the
/// question and key hint — sits on the bottom row with any lines naming
/// affected tasks stacked above it. Those rows are cleared first, so the
/// pane beneath doesn't show through to the right of a short line. When
/// `area` is shorter than the prompt, the leading lines are the ones cut
/// off, keeping the question visible. Lines are not wrapped.
fn draw_confirm(frame: &mut Frame, app: &App, area: Rect) {
    let Mode::Confirm { prompt, .. } = app.mode() else {
        return;
    };
    let line_count = u16::try_from(prompt.lines().count()).unwrap_or(u16::MAX);
    let height = line_count.min(area.height);
    let [_, prompt_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(height)]).areas(area);

    let paragraph = Paragraph::new(prompt.as_str())
        .style(Style::new().fg(Color::Yellow))
        .scroll((line_count - height, 0));
    frame.render_widget(Clear, prompt_area);
    frame.render_widget(paragraph, prompt_area);
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
/// Below them, when the task has any live predecessor, a "Blocked by:"
/// section lists each one's title and status, complete or not; under that,
/// when any live task depends on this one, a "Blocks:" section lists those
/// the same way. Neither section shows the dependency's type.
///
/// Renders nothing (an empty area) when there's no selected row — shouldn't
/// normally happen, since the Detail pane can only be entered from a
/// non-empty list, but avoids a panic if it ever does.
fn draw_detail(frame: &mut Frame, app: &App, area: Rect) {
    let Some(selected) = app.selected_row() else {
        return;
    };
    let description = app.description_of(selected.id).unwrap_or("");

    let blocked_by = related_task_lines("Blocked by:", app.predecessors());
    let blocks = related_task_lines("Blocks:", app.dependents());
    let [title_area, description_area, blocked_by_area, blocks_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(line_count(&blocked_by)),
        Constraint::Length(line_count(&blocks)),
    ])
    .areas(area);

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

    // A section with nothing to list has no lines and a zero-height area.
    frame.render_widget(Paragraph::new(blocked_by), blocked_by_area);
    frame.render_widget(Paragraph::new(blocks), blocks_area);
}

/// The lines of one Detail-pane dependency section: `heading`, then each of
/// `tasks` indented with its title and status. Empty when `tasks` is, so
/// the section is omitted entirely.
fn related_task_lines(heading: &'static str, tasks: &[RelatedTask]) -> Vec<Line<'static>> {
    if tasks.is_empty() {
        return Vec::new();
    }
    std::iter::once(Line::from(heading))
        .chain(
            tasks.iter().map(|task| {
                Line::from(format!("  {} ({})", task.title, status_label(task.status)))
            }),
        )
        .collect()
}

/// The number of rows `lines` occupies, saturating at `u16::MAX`.
fn line_count(lines: &[Line]) -> u16 {
    u16::try_from(lines.len()).unwrap_or(u16::MAX)
}

/// Lowercase display name of a task status.
fn status_label(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Incomplete => "incomplete",
        TaskStatus::Complete => "complete",
    }
}

#[cfg(test)]
mod tests {
    use bala_core::{TaskId, TaskStatus};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Modifier;

    use std::collections::HashMap;

    use bala_core::{Core, InMemoryStore};

    use super::{BLOCKED_GLYPH, MAX_INDENT_DEPTH, draw};
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
            blocked: false,
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
    fn draw_insert_should_label_the_reparent_line_with_a_parent_id_prompt() {
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
        let mut task_row = row("Write docs", TaskStatus::Incomplete);
        task_row.id = task.id;
        let mut app = App::new(vec![task_row]);
        let _ = apply_action(&mut app, &mut core, Action::StartReparent);

        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Parent id: _"), "got: {text}");
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
    fn draw_confirm_should_render_every_prompt_line_with_the_key_hint_last() {
        let mut core = core();
        let new_task = |title: &str| bala_core::NewTask {
            title: title.to_string(),
            description: None,
            parent_id: None,
            type_key: None,
            start_date: None,
            due_date: None,
            duration_days: None,
            assignee_id: None,
        };
        let a = core.create_task(new_task("A")).unwrap();
        // Enough long rows to fill the frame, so the prompt overlays list
        // rows rather than blank space.
        for n in 0..12 {
            let dependent = core
                .create_task(new_task(&format!("Dependent {n} with a long title")))
                .unwrap();
            if n < 2 {
                core.add_dependency(dependent.id, a.id, bala_core::DependencyType::default())
                    .unwrap();
            }
        }
        let tasks = core.get_tree(bala_core::TreeFilter::default()).unwrap();
        let order = core.sibling_order().unwrap();
        let rows = crate::render::task_rows(
            &tasks,
            &order,
            &HashMap::new(),
            &HashMap::new(),
            &std::collections::HashSet::new(),
        );
        let mut app = App::new(rows).with_tasks(tasks).with_sibling_order(order);
        app.select_by_id(Some(a.id));
        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);
        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);

        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let bottom_lines: Vec<String> = (6..10)
            .map(|y| {
                let line: String = (0..60).map(|x| buffer[(x, y)].symbol()).collect();
                line.trim_end().to_string()
            })
            .collect();
        assert_eq!(
            bottom_lines,
            [
                "2 task(s) depend on this task:",
                "  Dependent 0 with a long title",
                "  Dependent 1 with a long title",
                "Delete \"A\"? (y/n)",
            ]
        );
    }

    #[test]
    fn draw_confirm_should_keep_the_question_visible_when_the_prompt_is_taller_than_the_frame() {
        let mut core = core();
        let new_task = |title: &str| bala_core::NewTask {
            title: title.to_string(),
            description: None,
            parent_id: None,
            type_key: None,
            start_date: None,
            due_date: None,
            duration_days: None,
            assignee_id: None,
        };
        let a = core.create_task(new_task("A")).unwrap();
        for title in ["B", "C"] {
            let dependent = core.create_task(new_task(title)).unwrap();
            core.add_dependency(dependent.id, a.id, bala_core::DependencyType::default())
                .unwrap();
        }
        let tasks = core.get_tree(bala_core::TreeFilter::default()).unwrap();
        let order = core.sibling_order().unwrap();
        let rows = crate::render::task_rows(
            &tasks,
            &order,
            &HashMap::new(),
            &HashMap::new(),
            &std::collections::HashSet::new(),
        );
        let mut app = App::new(rows).with_tasks(tasks).with_sibling_order(order);
        app.select_by_id(Some(a.id));
        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);
        let _ = apply_action(&mut app, &mut core, Action::DKeyPressed);

        // Two rows for a four-line prompt.
        let mut terminal = Terminal::new(TestBackend::new(60, 2)).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let lines: Vec<String> = (0..2)
            .map(|y| {
                let line: String = (0..60).map(|x| buffer[(x, y)].symbol()).collect();
                line.trim_end().to_string()
            })
            .collect();
        assert_eq!(lines, ["  C", "Delete \"A\"? (y/n)"]);
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

        let backend = TestBackend::new(60, 30);
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
    fn draw_should_render_the_blocked_glyph_on_blocked_rows() {
        let free = row("Free task", TaskStatus::Incomplete);
        let mut blocked = row("Blocked task", TaskStatus::Incomplete);
        blocked.blocked = true;
        let app = App::new(vec![free, blocked]);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let row_text = |y: u16| -> String {
            (0..buffer.area().width)
                .map(|x| buffer.cell((x, y)).unwrap().symbol())
                .collect()
        };
        assert!(!row_text(0).contains(BLOCKED_GLYPH));
        assert!(row_text(1).contains(BLOCKED_GLYPH));
        assert!(row_text(1).contains("Blocked task"));
    }

    #[test]
    fn completing_a_predecessor_should_clear_the_blocked_glyph() {
        let mut core = core();
        let new_task = |title: &str| bala_core::NewTask {
            title: title.to_string(),
            description: None,
            parent_id: None,
            type_key: None,
            start_date: None,
            due_date: None,
            duration_days: None,
            assignee_id: None,
        };
        let predecessor = core.create_task(new_task("Predecessor")).unwrap();
        let successor = core.create_task(new_task("Successor")).unwrap();
        core.add_dependency(
            successor.id,
            predecessor.id,
            bala_core::DependencyType::default(),
        )
        .unwrap();
        let tasks = core.get_tree(bala_core::TreeFilter::default()).unwrap();
        let order = core.sibling_order().unwrap();
        let rows = crate::render::task_rows(
            &tasks,
            &order,
            &HashMap::new(),
            &HashMap::new(),
            &std::collections::HashSet::new(),
        );
        let mut app = App::new(rows).with_tasks(tasks).with_sibling_order(order);
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        assert!(buffer_text(terminal.backend().buffer()).contains(BLOCKED_GLYPH));
        assert!(app.rows()[1].blocked);

        app.select_by_id(Some(predecessor.id));
        let _ = apply_action(&mut app, &mut core, Action::ToggleComplete);

        assert!(!app.rows()[1].blocked);
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        assert!(!buffer_text(terminal.backend().buffer()).contains(BLOCKED_GLYPH));
    }

    #[test]
    fn draw_should_render_help_overlay_over_detail_pane_when_help_opened_from_detail() {
        let task_row = row("Write docs", TaskStatus::Incomplete);
        let mut app = App::new(vec![task_row]);
        let mut core = core();
        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);
        let _ = apply_action(&mut app, &mut core, Action::OpenHelp);

        let backend = TestBackend::new(60, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Esc — Leave the Detail pane, back to the list"));
        assert!(text.contains("i — Edit title"));
    }

    /// An app over a seeded core where "Ship it" depends on one task per
    /// `predecessors` entry (title and status), with "Ship it" selected and
    /// its Detail pane open.
    fn detail_app_with_predecessors(
        predecessors: &[(&str, TaskStatus)],
    ) -> (App, Core<InMemoryStore>) {
        let mut core = core();
        let new_task = |title: &str| bala_core::NewTask {
            title: title.to_string(),
            description: None,
            parent_id: None,
            type_key: None,
            start_date: None,
            due_date: None,
            duration_days: None,
            assignee_id: None,
        };
        let mut preds = Vec::new();
        for (title, status) in predecessors {
            let pred = core.create_task(new_task(title)).unwrap();
            if *status == TaskStatus::Complete {
                core.complete_task(pred.id, false).unwrap();
            }
            preds.push(pred);
        }
        let successor = core.create_task(new_task("Ship it")).unwrap();
        for pred in &preds {
            core.add_dependency(successor.id, pred.id, bala_core::DependencyType::default())
                .unwrap();
        }
        let tasks = core.get_tree(bala_core::TreeFilter::default()).unwrap();
        let order = core.sibling_order().unwrap();
        let rows = crate::render::task_rows(
            &tasks,
            &order,
            &HashMap::new(),
            &HashMap::new(),
            &std::collections::HashSet::new(),
        );
        let mut app = App::new(rows).with_tasks(tasks).with_sibling_order(order);
        app.select_by_id(Some(successor.id));
        // Entering the Detail pane through `apply_action` is what loads the
        // selected row's predecessors.
        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);
        (app, core)
    }

    #[test]
    fn draw_detail_should_list_each_predecessor_with_title_and_status() {
        let (app, _core) = detail_app_with_predecessors(&[
            ("Design", TaskStatus::Complete),
            ("Review", TaskStatus::Incomplete),
        ]);
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();

        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Blocked by:"));
        assert!(text.contains("  Design (complete)"));
        assert!(text.contains("  Review (incomplete)"));
    }

    #[test]
    fn draw_detail_should_omit_blocked_by_when_the_task_has_no_predecessors() {
        let (app, _core) = detail_app_with_predecessors(&[]);
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();

        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Ship it"));
        assert!(!text.contains("Blocked by"));
    }

    /// An app over a seeded core where one task per `dependents` entry
    /// (title and status) depends on "Design", with "Design" selected and
    /// its Detail pane open.
    fn detail_app_with_dependents(dependents: &[(&str, TaskStatus)]) -> (App, Core<InMemoryStore>) {
        let mut core = core();
        let new_task = |title: &str| bala_core::NewTask {
            title: title.to_string(),
            description: None,
            parent_id: None,
            type_key: None,
            start_date: None,
            due_date: None,
            duration_days: None,
            assignee_id: None,
        };
        let predecessor = core.create_task(new_task("Design")).unwrap();
        for (title, status) in dependents {
            let dependent = core.create_task(new_task(title)).unwrap();
            // Completed before the edge exists, while nothing blocks it.
            if *status == TaskStatus::Complete {
                core.complete_task(dependent.id, false).unwrap();
            }
            core.add_dependency(
                dependent.id,
                predecessor.id,
                bala_core::DependencyType::default(),
            )
            .unwrap();
        }
        let tasks = core.get_tree(bala_core::TreeFilter::default()).unwrap();
        let order = core.sibling_order().unwrap();
        let rows = crate::render::task_rows(
            &tasks,
            &order,
            &HashMap::new(),
            &HashMap::new(),
            &std::collections::HashSet::new(),
        );
        let mut app = App::new(rows).with_tasks(tasks).with_sibling_order(order);
        app.select_by_id(Some(predecessor.id));
        // Entering the Detail pane through `apply_action` is what loads the
        // selected row's dependents.
        let _ = apply_action(&mut app, &mut core, Action::EnterDetail);
        (app, core)
    }

    #[test]
    fn draw_detail_should_list_each_dependent_with_title_and_status() {
        let (app, _core) = detail_app_with_dependents(&[
            ("Build", TaskStatus::Incomplete),
            ("Announce", TaskStatus::Complete),
        ]);
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();

        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Blocks:"));
        assert!(text.contains("  Build (incomplete)"));
        assert!(text.contains("  Announce (complete)"));
        assert!(!text.contains("Blocked by"));
    }

    #[test]
    fn draw_detail_should_omit_blocks_when_nothing_depends_on_the_task() {
        let (app, _core) = detail_app_with_dependents(&[]);
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();

        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Design"));
        assert!(!text.contains("Blocks"));
    }

    #[test]
    fn draw_should_show_the_active_blocked_view_in_the_status_line() {
        let mut app = App::new(vec![row("Write docs", TaskStatus::Incomplete)]);
        let mut core = core();
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| draw(frame, &app)).unwrap();
        assert!(!buffer_text(terminal.backend().buffer()).contains("view:"));

        let _ = apply_action(&mut app, &mut core, Action::CycleBlockedView);
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        assert!(buffer_text(terminal.backend().buffer()).contains("view: blocked"));

        let _ = apply_action(&mut app, &mut core, Action::CycleBlockedView);
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        assert!(buffer_text(terminal.backend().buffer()).contains("view: ready"));
    }

    #[test]
    fn draw_should_list_the_blocked_view_key_in_the_help_overlay() {
        let mut app = App::new(vec![row("Write docs", TaskStatus::Incomplete)]);
        let mut core = core();
        let _ = apply_action(&mut app, &mut core, Action::OpenHelp);
        let backend = TestBackend::new(60, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("b — Cycle the view: all, blocked, ready"));
    }

    #[test]
    fn draw_insert_should_label_the_add_dependency_line() {
        let mut app = App::new(vec![row("Write docs", TaskStatus::Incomplete)]);
        let mut core = core();
        let _ = apply_action(&mut app, &mut core, Action::StartAddDependency);

        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("Depends on (task id): _"), "got: {text}");
    }

    #[test]
    fn draw_insert_should_label_the_remove_dependency_line() {
        // Two predecessors, so the line opens empty rather than prefilled.
        let (mut app, mut core) = detail_app_with_predecessors(&[
            ("Design", TaskStatus::Incomplete),
            ("Review", TaskStatus::Incomplete),
        ]);
        let _ = apply_action(&mut app, &mut core, Action::StartRemoveDependency);

        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let text = buffer_text(terminal.backend().buffer());
        assert!(
            text.contains("Remove dependency on (task id): _"),
            "got: {text}"
        );
    }
}
