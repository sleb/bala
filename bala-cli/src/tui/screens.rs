//! Renders `App`'s selection state to a ratatui frame.
//!
//! Kept separate from `app` so drawing logic can be exercised against
//! `ratatui::backend::TestBackend` (no real terminal) before the real
//! crossterm event loop wires it up.

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};

use crate::tui::app::App;

/// Draws the current `App` state into `frame`.
///
/// Renders a centered "No tasks yet." message when there are no rows,
/// otherwise a `List` of one line per row with the selected row
/// highlighted.
pub fn draw(frame: &mut Frame, app: &App) {
    if app.rows().is_empty() {
        draw_empty(frame, frame.area());
        return;
    }

    let items: Vec<ListItem> = app
        .rows()
        .iter()
        .map(|row| {
            ListItem::new(format!(
                "{}  [{}]  {:?}  {}",
                row.title,
                row.type_label,
                row.status,
                row.assignee_name.as_deref().unwrap_or("Unassigned")
            ))
        })
        .collect();

    let list = List::new(items).highlight_style(Style::new().add_modifier(Modifier::REVERSED));

    let mut state = ListState::default();
    state.select(app.selected_index());

    frame.render_stateful_widget(list, frame.area(), &mut state);
}

/// Renders the "No tasks yet." message, centered within `area`.
fn draw_empty(frame: &mut Frame, area: Rect) {
    let paragraph = Paragraph::new("No tasks yet.")
        .alignment(Alignment::Center)
        .block(Block::default());
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use bala_core::{TaskId, TaskStatus};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Modifier;

    use super::draw;
    use crate::render::TaskRow;
    use crate::tui::app::App;

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
    fn draw_should_render_each_row_title_and_status() {
        let rows = vec![
            row("First task", TaskStatus::Incomplete),
            row("Second task", TaskStatus::Complete),
        ];
        let app = App::new(rows);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| draw(frame, &app)).unwrap();

        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("First task"));
        assert!(text.contains("Second task"));
        assert!(text.contains("Incomplete"));
        assert!(text.contains("Complete"));
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
}
