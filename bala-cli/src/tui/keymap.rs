//! Maps a `KeyEvent` to an `Action`, given the current `Mode`, `Pane`, and
//! (when in the Detail pane) `DetailField` cursor.
//!
//! Keeping this mapping separate from `App`'s state mutation
//! (`app::apply_action`) means the same physical key can mean different
//! things in different modes/panes without a giant flat `match` spread
//! across mode-aware branches inside the mutation logic itself.

use crossterm::event::{KeyCode, KeyEvent};

use crate::tui::mode::{DetailField, Mode, Pane};

/// An intent derived from a key event and the current mode/pane, ready to be
/// applied to `App`/`Core` by `app::apply_action`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    MoveDown,
    MoveUp,
    Quit,
    /// `Normal` mode, `O`: start creating a new top-level task's title.
    StartInsertNewTitle,
    /// `Normal` mode, `List` pane, `Enter`: open the Detail pane on the
    /// focused task.
    EnterDetail,
    /// `Normal` mode, `Detail` pane, `Esc`: back out to the `List` pane.
    LeaveDetail,
    /// `Normal` mode, `Detail` pane, `j`/`Down`: move the detail field
    /// cursor to the next field.
    DetailCursorDown,
    /// `Normal` mode, `Detail` pane, `k`/`Up`: move the detail field cursor
    /// to the previous field.
    DetailCursorUp,
    /// `Normal` mode, `Detail` pane, `i` with the title field under the
    /// cursor: start editing the focused task's title.
    StartEditTitle,
    /// `Normal` mode, `Detail` pane, `i` with the description field under
    /// the cursor: start editing the focused task's description.
    StartEditDescription,
    /// `Insert` mode: append a character to the edit buffer.
    InsertChar(char),
    /// `Insert` mode: remove the last character from the edit buffer.
    Backspace,
    /// `Insert` mode, `Esc`: discard the in-progress edit and return to
    /// `Normal`.
    CancelInsert,
    /// `Insert` mode, `Enter`: submit the edit buffer.
    SubmitInsert,
    /// `Normal` mode, `List` pane, `d`: one `d` key press. `App::apply_action`
    /// decides whether this is the first or second of a `dd` sequence.
    DKeyPressed,
    /// `Confirm` mode, `y`: run the pending action.
    ConfirmYes,
    /// `Confirm` mode, `n`/`Esc`: cancel back to `Normal`.
    ConfirmNo,
    /// No mapping for this key in this mode/pane.
    Noop,
}

/// Maps `key` to an `Action`, given the current `mode`, `pane`, and (when in
/// the Detail pane) `detail_field` cursor.
///
/// `Esc` is universal in `Mode::Insert`: it always maps to `CancelInsert`
/// regardless of which field is being edited. `Esc` in `Mode::Normal`'s
/// `Detail` pane maps to `LeaveDetail` — a generalization of that same "Esc
/// backs out" rule to pane navigation, since Detail isn't itself a `Mode`.
#[must_use]
pub fn key_to_action(mode: &Mode, pane: Pane, detail_field: DetailField, key: KeyEvent) -> Action {
    match mode {
        Mode::Normal => match pane {
            Pane::List => match key.code {
                KeyCode::Char('j') | KeyCode::Down => Action::MoveDown,
                KeyCode::Char('k') | KeyCode::Up => Action::MoveUp,
                KeyCode::Char('q') => Action::Quit,
                KeyCode::Char('O') => Action::StartInsertNewTitle,
                KeyCode::Enter => Action::EnterDetail,
                KeyCode::Char('d') => Action::DKeyPressed,
                _ => Action::Noop,
            },
            Pane::Detail => match key.code {
                KeyCode::Esc => Action::LeaveDetail,
                KeyCode::Char('j') | KeyCode::Down => Action::DetailCursorDown,
                KeyCode::Char('k') | KeyCode::Up => Action::DetailCursorUp,
                KeyCode::Char('q') => Action::Quit,
                KeyCode::Char('i') => match detail_field {
                    DetailField::Title => Action::StartEditTitle,
                    DetailField::Description => Action::StartEditDescription,
                },
                _ => Action::Noop,
            },
        },
        Mode::Insert { .. } => match key.code {
            KeyCode::Esc => Action::CancelInsert,
            KeyCode::Enter => Action::SubmitInsert,
            KeyCode::Backspace => Action::Backspace,
            KeyCode::Char(c) => Action::InsertChar(c),
            _ => Action::Noop,
        },
        Mode::Confirm { .. } => match key.code {
            KeyCode::Char('y') => Action::ConfirmYes,
            KeyCode::Char('n') | KeyCode::Esc => Action::ConfirmNo,
            _ => Action::Noop,
        },
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{Action, key_to_action};
    use crate::tui::mode::{DetailField, EditableField, Mode, Pane};

    #[test]
    fn key_to_action_should_map_uppercase_o_in_normal_list_to_start_insert_new_title() {
        let action = key_to_action(
            &Mode::Normal,
            Pane::List,
            DetailField::Title,
            KeyEvent::new(KeyCode::Char('O'), KeyModifiers::SHIFT),
        );

        assert_eq!(action, Action::StartInsertNewTitle);
    }

    #[test]
    fn key_to_action_should_map_char_in_insert_mode_to_insert_char() {
        let mode = Mode::Insert {
            field: EditableField::NewTitle,
            buffer: String::new(),
        };

        let action = key_to_action(
            &mode,
            Pane::List,
            DetailField::Title,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );

        assert_eq!(action, Action::InsertChar('x'));
    }

    #[test]
    fn key_to_action_should_map_backspace_in_insert_mode_to_backspace() {
        let mode = Mode::Insert {
            field: EditableField::NewTitle,
            buffer: "abc".to_string(),
        };

        let action = key_to_action(
            &mode,
            Pane::List,
            DetailField::Title,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        );

        assert_eq!(action, Action::Backspace);
    }

    #[test]
    fn key_to_action_should_map_esc_in_insert_mode_to_cancel_insert() {
        let mode = Mode::Insert {
            field: EditableField::NewTitle,
            buffer: "abc".to_string(),
        };

        let action = key_to_action(
            &mode,
            Pane::Detail,
            DetailField::Title,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        assert_eq!(action, Action::CancelInsert);
    }

    #[test]
    fn key_to_action_should_map_enter_in_insert_mode_to_submit_insert() {
        let mode = Mode::Insert {
            field: EditableField::NewTitle,
            buffer: "abc".to_string(),
        };

        let action = key_to_action(
            &mode,
            Pane::List,
            DetailField::Title,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        assert_eq!(action, Action::SubmitInsert);
    }

    #[test]
    fn key_to_action_should_map_enter_in_normal_list_pane_to_enter_detail() {
        let action = key_to_action(
            &Mode::Normal,
            Pane::List,
            DetailField::Title,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        assert_eq!(action, Action::EnterDetail);
    }

    #[test]
    fn key_to_action_should_map_esc_in_normal_detail_pane_to_leave_detail() {
        let action = key_to_action(
            &Mode::Normal,
            Pane::Detail,
            DetailField::Title,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        assert_eq!(action, Action::LeaveDetail);
    }

    #[test]
    fn key_to_action_should_map_j_k_in_detail_pane_to_move_detail_cursor() {
        let down = key_to_action(
            &Mode::Normal,
            Pane::Detail,
            DetailField::Title,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        );
        let up = key_to_action(
            &Mode::Normal,
            Pane::Detail,
            DetailField::Description,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
        );

        assert_eq!(down, Action::DetailCursorDown);
        assert_eq!(up, Action::DetailCursorUp);
    }

    #[test]
    fn key_to_action_should_map_i_on_title_cursor_to_start_edit_title() {
        let action = key_to_action(
            &Mode::Normal,
            Pane::Detail,
            DetailField::Title,
            KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
        );

        assert_eq!(action, Action::StartEditTitle);
    }

    #[test]
    fn key_to_action_should_map_i_on_description_cursor_to_start_edit_description() {
        let action = key_to_action(
            &Mode::Normal,
            Pane::Detail,
            DetailField::Description,
            KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
        );

        assert_eq!(action, Action::StartEditDescription);
    }

    #[test]
    fn key_to_action_should_map_d_in_normal_mode_to_d_key_pressed() {
        let action = key_to_action(
            &Mode::Normal,
            Pane::List,
            DetailField::Title,
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
        );

        assert_eq!(action, Action::DKeyPressed);
    }

    #[test]
    fn key_to_action_should_map_y_in_confirm_mode_to_confirm_yes() {
        let mode = Mode::Confirm {
            prompt: "Delete \"Task\"? (y/n)".to_string(),
            action: crate::tui::mode::PendingAction::Delete(bala_core::TaskId::new()),
        };

        let action = key_to_action(
            &mode,
            Pane::List,
            DetailField::Title,
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
        );

        assert_eq!(action, Action::ConfirmYes);
    }

    #[test]
    fn key_to_action_should_map_n_in_confirm_mode_to_confirm_no() {
        let mode = Mode::Confirm {
            prompt: "Delete \"Task\"? (y/n)".to_string(),
            action: crate::tui::mode::PendingAction::Delete(bala_core::TaskId::new()),
        };

        let action = key_to_action(
            &mode,
            Pane::List,
            DetailField::Title,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
        );

        assert_eq!(action, Action::ConfirmNo);
    }

    #[test]
    fn key_to_action_should_map_esc_in_confirm_mode_to_confirm_no() {
        let mode = Mode::Confirm {
            prompt: "Delete \"Task\"? (y/n)".to_string(),
            action: crate::tui::mode::PendingAction::Delete(bala_core::TaskId::new()),
        };

        let action = key_to_action(
            &mode,
            Pane::List,
            DetailField::Title,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        assert_eq!(action, Action::ConfirmNo);
    }
}
