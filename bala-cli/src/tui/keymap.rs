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
    /// `Normal` mode, `List` pane, `o`: start creating a new subtask under
    /// the focused task.
    StartInsertNewSubtask,
    /// `Normal` mode, `List` pane, `m`: start reparenting the focused task.
    StartReparent,
    /// `Normal` mode, `List` pane, `t`: start setting the focused task's
    /// type.
    StartSetType,
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
    /// `Normal` mode, `List`/`Detail` pane, `x`/`Space`: toggle the selected
    /// task's complete/incomplete state.
    ToggleComplete,
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
    /// `Normal` mode (either pane) or `Confirm` mode, `?`; `Insert` mode,
    /// `F1`: open the keybinding help overlay.
    OpenHelp,
    /// `Help` mode, `Esc`: close the overlay, restoring the previous mode.
    CloseHelp,
    /// `Normal` mode, `List` pane, `h`/`←`: collapse the focused task, hiding
    /// its descendants.
    CollapseFocused,
    /// `Normal` mode, `List` pane, `l`/`→`: expand the focused task,
    /// revealing its previously hidden descendants.
    ExpandFocused,
    /// `Normal` mode, `List` pane, `E`: expand every collapsed task.
    ExpandAll,
    /// `Normal` mode, `List` pane, `C`: collapse every task that has
    /// children.
    CollapseAll,
    /// `Normal` mode, `List` pane, `f`: cycle the active type filter.
    CycleTypeFilter,
    /// `Normal` mode, `List` pane, `J`: move the focused task down among its
    /// siblings (under the parent it is rendered beneath).
    MoveTaskDown,
    /// `Normal` mode, `List` pane, `K`: move the focused task up among its
    /// siblings (under the parent it is rendered beneath).
    MoveTaskUp,
    /// `Normal` mode, `List` pane, `L`: indent the focused task under its
    /// previous sibling (it becomes that sibling's last child).
    IndentTask,
    /// `Normal` mode, `List` pane, `H`: outdent the focused task to its
    /// parent's level, right after that parent.
    OutdentTask,
    /// No mapping for this key in this mode/pane.
    Noop,
}

/// A single key binding: the physical key(s) that trigger `action`, plus the
/// human-facing text [`help_entries`] shows for it.
///
/// `label`/`description` are unused by dispatch itself (see
/// [`key_to_action`]) but let [`help_entries`] read them straight off these
/// tables rather than maintaining a hand-written second copy of the keymap.
#[derive(Debug, Clone, Copy)]
pub struct Binding {
    /// The key(s) that trigger `action`. Multiple keys may map to the same
    /// action (e.g. `j` and `Down` both move down).
    pub keys: &'static [KeyCode],
    /// The action this binding produces.
    pub action: Action,
    /// Short human-facing label for the key(s), e.g. `"j/↓"`.
    pub label: &'static str,
    /// Human-facing description of what the action does.
    pub description: &'static str,
}

/// Bindings active in `Mode::Normal`, `Pane::List`.
pub static NORMAL_LIST_BINDINGS: &[Binding] = &[
    Binding {
        keys: &[KeyCode::Char('j'), KeyCode::Down],
        action: Action::MoveDown,
        label: "j/↓",
        description: "Move the selection down",
    },
    Binding {
        keys: &[KeyCode::Char('k'), KeyCode::Up],
        action: Action::MoveUp,
        label: "k/↑",
        description: "Move the selection up",
    },
    Binding {
        keys: &[KeyCode::Char('q')],
        action: Action::Quit,
        label: "q",
        description: "Quit, restoring the terminal to its normal state",
    },
    Binding {
        keys: &[KeyCode::Char('O')],
        action: Action::StartInsertNewTitle,
        label: "O",
        description: "Create a new top-level task",
    },
    Binding {
        keys: &[KeyCode::Char('o')],
        action: Action::StartInsertNewSubtask,
        label: "o",
        description: "Create a new subtask under the selected task",
    },
    Binding {
        keys: &[KeyCode::Char('m')],
        action: Action::StartReparent,
        label: "m",
        description: "Reparent the selected task",
    },
    Binding {
        keys: &[KeyCode::Enter],
        action: Action::EnterDetail,
        label: "Enter",
        description: "Open the Detail pane for the selected task",
    },
    Binding {
        keys: &[KeyCode::Char('d')],
        action: Action::DKeyPressed,
        label: "dd",
        description: "Delete the selected task (press d twice)",
    },
    Binding {
        keys: &[KeyCode::Char('x'), KeyCode::Char(' ')],
        action: Action::ToggleComplete,
        label: "x/Space",
        description: "Toggle complete/incomplete",
    },
    Binding {
        keys: &[KeyCode::Char('?')],
        action: Action::OpenHelp,
        label: "?",
        description: "Show the keybinding help overlay",
    },
    Binding {
        keys: &[KeyCode::Char('h'), KeyCode::Left],
        action: Action::CollapseFocused,
        label: "h/←",
        description: "Collapse the focused task",
    },
    Binding {
        keys: &[KeyCode::Char('l'), KeyCode::Right],
        action: Action::ExpandFocused,
        label: "l/→",
        description: "Expand the focused task",
    },
    Binding {
        keys: &[KeyCode::Char('E')],
        action: Action::ExpandAll,
        label: "E",
        description: "Expand all tasks",
    },
    Binding {
        keys: &[KeyCode::Char('C')],
        action: Action::CollapseAll,
        label: "C",
        description: "Collapse all tasks",
    },
    Binding {
        keys: &[KeyCode::Char('J')],
        action: Action::MoveTaskDown,
        label: "J",
        description: "Move task down among siblings",
    },
    Binding {
        keys: &[KeyCode::Char('K')],
        action: Action::MoveTaskUp,
        label: "K",
        description: "Move task up among siblings",
    },
    Binding {
        keys: &[KeyCode::Char('L')],
        action: Action::IndentTask,
        label: "L",
        description: "Indent task under previous sibling",
    },
    Binding {
        keys: &[KeyCode::Char('H')],
        action: Action::OutdentTask,
        label: "H",
        description: "Outdent task to parent's level",
    },
    Binding {
        keys: &[KeyCode::Char('t')],
        action: Action::StartSetType,
        label: "t",
        description: "Set the selected task's type",
    },
    Binding {
        keys: &[KeyCode::Char('f')],
        action: Action::CycleTypeFilter,
        label: "f",
        description: "Cycle the active type filter",
    },
];

/// Bindings active in `Mode::Normal`, `Pane::Detail`.
///
/// Detail's `i` key is not listed here: it maps to `StartEditTitle` or
/// `StartEditDescription` depending on `detail_field`, so it isn't a fixed
/// key-to-action binding and is special-cased in [`key_to_action`] instead.
pub static NORMAL_DETAIL_BINDINGS: &[Binding] = &[
    Binding {
        keys: &[KeyCode::Esc],
        action: Action::LeaveDetail,
        label: "Esc",
        description: "Leave the Detail pane, back to the list",
    },
    Binding {
        keys: &[KeyCode::Char('j'), KeyCode::Down],
        action: Action::DetailCursorDown,
        label: "j/↓",
        description: "Move the field cursor down",
    },
    Binding {
        keys: &[KeyCode::Char('k'), KeyCode::Up],
        action: Action::DetailCursorUp,
        label: "k/↑",
        description: "Move the field cursor up",
    },
    Binding {
        keys: &[KeyCode::Char('q')],
        action: Action::Quit,
        label: "q",
        description: "Quit, restoring the terminal to its normal state",
    },
    Binding {
        keys: &[KeyCode::Char('x'), KeyCode::Char(' ')],
        action: Action::ToggleComplete,
        label: "x/Space",
        description: "Toggle complete/incomplete",
    },
    Binding {
        keys: &[KeyCode::Char('?')],
        action: Action::OpenHelp,
        label: "?",
        description: "Show the keybinding help overlay",
    },
];

/// Bindings active in `Mode::Insert`.
///
/// The generic `Char(c)` catch-all (any other character maps to
/// `Action::InsertChar(c)`) isn't a fixed key either, so it's handled as a
/// fallback in [`key_to_action`] after this table.
pub static INSERT_BINDINGS: &[Binding] = &[
    Binding {
        keys: &[KeyCode::Esc],
        action: Action::CancelInsert,
        label: "Esc",
        description: "Discard the edit and return",
    },
    Binding {
        keys: &[KeyCode::Enter],
        action: Action::SubmitInsert,
        label: "Enter",
        description: "Submit the edit",
    },
    Binding {
        keys: &[KeyCode::Backspace],
        action: Action::Backspace,
        label: "Backspace",
        description: "Remove the last character",
    },
    Binding {
        keys: &[KeyCode::F(1)],
        action: Action::OpenHelp,
        label: "F1",
        description: "Show the keybinding help overlay",
    },
];

/// Bindings active in `Mode::Confirm`.
pub static CONFIRM_BINDINGS: &[Binding] = &[
    Binding {
        keys: &[KeyCode::Char('y')],
        action: Action::ConfirmYes,
        label: "y",
        description: "Confirm",
    },
    Binding {
        keys: &[KeyCode::Char('n'), KeyCode::Esc],
        action: Action::ConfirmNo,
        label: "n/Esc",
        description: "Cancel",
    },
    Binding {
        keys: &[KeyCode::Char('?')],
        action: Action::OpenHelp,
        label: "?",
        description: "Show the keybinding help overlay",
    },
];

/// Looks up `code` in `bindings`, returning the first matching binding's
/// action, or `None` if no binding covers `code`.
fn lookup(bindings: &[Binding], code: KeyCode) -> Option<Action> {
    bindings
        .iter()
        .find(|binding| binding.keys.contains(&code))
        .map(|binding| binding.action)
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
            Pane::List => lookup(NORMAL_LIST_BINDINGS, key.code).unwrap_or(Action::Noop),
            Pane::Detail => {
                if key.code == KeyCode::Char('i') {
                    return match detail_field {
                        DetailField::Title => Action::StartEditTitle,
                        DetailField::Description => Action::StartEditDescription,
                    };
                }
                lookup(NORMAL_DETAIL_BINDINGS, key.code).unwrap_or(Action::Noop)
            }
        },
        Mode::Insert { .. } => lookup(INSERT_BINDINGS, key.code).unwrap_or(match key.code {
            KeyCode::Char(c) => Action::InsertChar(c),
            _ => Action::Noop,
        }),
        Mode::Confirm { .. } => lookup(CONFIRM_BINDINGS, key.code).unwrap_or(Action::Noop),
        Mode::Help { .. } => match key.code {
            KeyCode::Esc => Action::CloseHelp,
            _ => Action::Noop,
        },
    }
}

/// The description shown for the Detail pane's `i` key given which field is
/// under the cursor.
///
/// This matches on the same [`DetailField`] variants that [`key_to_action`]'s
/// Detail-pane `i` handling matches on, so adding a new `DetailField` variant
/// makes both `match`es non-exhaustive and fail to compile until both are
/// updated — the two descriptions of "what `i` does" can't silently drift
/// apart.
fn detail_edit_label(field: DetailField) -> &'static str {
    match field {
        DetailField::Title => "Edit title",
        DetailField::Description => "Edit description",
    }
}

/// One row of a help overlay listing: a key label and what it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HelpEntry {
    /// Short human-facing label for the key(s), e.g. `"j/↓"`.
    pub key: &'static str,
    /// Human-facing description of what the action does.
    pub description: &'static str,
}

impl From<&Binding> for HelpEntry {
    fn from(binding: &Binding) -> Self {
        HelpEntry {
            key: binding.label,
            description: binding.description,
        }
    }
}

/// Builds the help overlay's key listing for the mode/pane/field the overlay
/// is showing keys for.
///
/// `mode` is the mode `Mode::Help`'s `previous` holds, not `Help` itself:
/// callers pass along whatever mode Help is fronting for so the listing
/// reflects the keys that will apply once the overlay is closed. Entries are
/// generated straight from the same [`Binding`] tables [`key_to_action`]
/// dispatches from, so the listing can't drift from actual dispatch.
#[must_use]
pub fn help_entries(mode: &Mode, pane: Pane, detail_field: DetailField) -> Vec<HelpEntry> {
    match mode {
        Mode::Normal => match pane {
            Pane::List => NORMAL_LIST_BINDINGS.iter().map(HelpEntry::from).collect(),
            Pane::Detail => {
                let mut entries: Vec<HelpEntry> =
                    NORMAL_DETAIL_BINDINGS.iter().map(HelpEntry::from).collect();
                entries.push(HelpEntry {
                    key: "i",
                    description: detail_edit_label(detail_field),
                });
                entries
            }
        },
        Mode::Insert { .. } => INSERT_BINDINGS.iter().map(HelpEntry::from).collect(),
        Mode::Confirm { .. } => CONFIRM_BINDINGS.iter().map(HelpEntry::from).collect(),
        Mode::Help { .. } => vec![],
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{Action, help_entries, key_to_action};
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
    fn key_to_action_should_map_uppercase_j_and_k_to_move_task() {
        let map = |c| {
            key_to_action(
                &Mode::Normal,
                Pane::List,
                DetailField::Title,
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT),
            )
        };

        assert_eq!(map('J'), Action::MoveTaskDown);
        assert_eq!(map('K'), Action::MoveTaskUp);
    }

    #[test]
    fn key_to_action_should_map_uppercase_h_and_l_to_outdent_and_indent() {
        let map = |c| {
            key_to_action(
                &Mode::Normal,
                Pane::List,
                DetailField::Title,
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT),
            )
        };

        assert_eq!(map('L'), Action::IndentTask);
        assert_eq!(map('H'), Action::OutdentTask);
    }

    #[test]
    fn help_overlay_should_list_indent_outdent_keys() {
        let entries = help_entries(&Mode::Normal, Pane::List, DetailField::Title);

        assert!(
            entries
                .iter()
                .any(|e| e.key == "L" && e.description == "Indent task under previous sibling")
        );
        assert!(
            entries
                .iter()
                .any(|e| e.key == "H" && e.description == "Outdent task to parent's level")
        );
    }

    #[test]
    fn key_to_action_should_not_map_uppercase_j_in_detail_pane() {
        let action = key_to_action(
            &Mode::Normal,
            Pane::Detail,
            DetailField::Title,
            KeyEvent::new(KeyCode::Char('J'), KeyModifiers::SHIFT),
        );

        assert_eq!(action, Action::Noop);
    }

    #[test]
    fn help_overlay_should_list_move_keys() {
        let entries = help_entries(&Mode::Normal, Pane::List, DetailField::Title);

        assert!(
            entries
                .iter()
                .any(|e| e.key == "J" && e.description == "Move task down among siblings")
        );
        assert!(
            entries
                .iter()
                .any(|e| e.key == "K" && e.description == "Move task up among siblings")
        );
    }

    #[test]
    fn key_to_action_should_map_t_in_normal_list_to_start_set_type() {
        let action = key_to_action(
            &Mode::Normal,
            Pane::List,
            DetailField::Title,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE),
        );

        assert_eq!(action, Action::StartSetType);
    }

    #[test]
    fn key_to_action_should_map_f_in_normal_list_to_cycle_type_filter() {
        let action = key_to_action(
            &Mode::Normal,
            Pane::List,
            DetailField::Title,
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE),
        );

        assert_eq!(action, Action::CycleTypeFilter);
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
    fn key_to_action_should_map_x_in_normal_list_to_toggle_complete() {
        let action = key_to_action(
            &Mode::Normal,
            Pane::List,
            DetailField::Title,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );

        assert_eq!(action, Action::ToggleComplete);
    }

    #[test]
    fn key_to_action_should_map_space_in_normal_list_to_toggle_complete() {
        let action = key_to_action(
            &Mode::Normal,
            Pane::List,
            DetailField::Title,
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
        );

        assert_eq!(action, Action::ToggleComplete);
    }

    #[test]
    fn key_to_action_should_map_x_in_normal_detail_pane_to_toggle_complete() {
        let action = key_to_action(
            &Mode::Normal,
            Pane::Detail,
            DetailField::Title,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );

        assert_eq!(action, Action::ToggleComplete);
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

    #[test]
    fn key_to_action_should_map_question_mark_in_normal_list_to_open_help() {
        let action = key_to_action(
            &Mode::Normal,
            Pane::List,
            DetailField::Title,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        );

        assert_eq!(action, Action::OpenHelp);
    }

    #[test]
    fn key_to_action_should_map_question_mark_in_normal_detail_to_open_help() {
        let action = key_to_action(
            &Mode::Normal,
            Pane::Detail,
            DetailField::Title,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        );

        assert_eq!(action, Action::OpenHelp);
    }

    #[test]
    fn key_to_action_should_map_question_mark_in_confirm_mode_to_open_help() {
        let mode = Mode::Confirm {
            prompt: "Delete \"Task\"? (y/n)".to_string(),
            action: crate::tui::mode::PendingAction::Delete(bala_core::TaskId::new()),
        };

        let action = key_to_action(
            &mode,
            Pane::List,
            DetailField::Title,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        );

        assert_eq!(action, Action::OpenHelp);
    }

    #[test]
    fn key_to_action_should_map_f1_in_insert_mode_to_open_help() {
        let mode = Mode::Insert {
            field: EditableField::NewTitle,
            buffer: String::new(),
        };

        let action = key_to_action(
            &mode,
            Pane::List,
            DetailField::Title,
            KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE),
        );

        assert_eq!(action, Action::OpenHelp);
    }

    #[test]
    fn key_to_action_should_still_map_question_mark_char_in_insert_mode_to_insert_char() {
        let mode = Mode::Insert {
            field: EditableField::NewTitle,
            buffer: String::new(),
        };

        let action = key_to_action(
            &mode,
            Pane::List,
            DetailField::Title,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        );

        assert_eq!(action, Action::InsertChar('?'));
    }

    #[test]
    fn key_to_action_should_map_esc_in_help_mode_to_close_help() {
        let mode = Mode::Help {
            previous: Box::new(Mode::Normal),
        };

        let action = key_to_action(
            &mode,
            Pane::List,
            DetailField::Title,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        assert_eq!(action, Action::CloseHelp);
    }

    #[test]
    fn key_to_action_should_map_h_and_left_to_collapse_focused() {
        let via_h = key_to_action(
            &Mode::Normal,
            Pane::List,
            DetailField::Title,
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
        );
        let via_left = key_to_action(
            &Mode::Normal,
            Pane::List,
            DetailField::Title,
            KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
        );

        assert_eq!(via_h, Action::CollapseFocused);
        assert_eq!(via_left, Action::CollapseFocused);
    }

    #[test]
    fn key_to_action_should_map_l_and_right_to_expand_focused() {
        let via_l = key_to_action(
            &Mode::Normal,
            Pane::List,
            DetailField::Title,
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
        );
        let via_right = key_to_action(
            &Mode::Normal,
            Pane::List,
            DetailField::Title,
            KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        );

        assert_eq!(via_l, Action::ExpandFocused);
        assert_eq!(via_right, Action::ExpandFocused);
    }

    #[test]
    fn key_to_action_should_map_uppercase_e_to_expand_all() {
        let action = key_to_action(
            &Mode::Normal,
            Pane::List,
            DetailField::Title,
            KeyEvent::new(KeyCode::Char('E'), KeyModifiers::SHIFT),
        );

        assert_eq!(action, Action::ExpandAll);
    }

    #[test]
    fn key_to_action_should_map_uppercase_c_to_collapse_all() {
        let action = key_to_action(
            &Mode::Normal,
            Pane::List,
            DetailField::Title,
            KeyEvent::new(KeyCode::Char('C'), KeyModifiers::SHIFT),
        );

        assert_eq!(action, Action::CollapseAll);
    }

    #[test]
    fn help_entries_for_normal_list_should_include_collapse_expand_bindings() {
        let entries = help_entries(&Mode::Normal, Pane::List, DetailField::Title);

        assert!(
            entries
                .iter()
                .any(|e| e.key == "h/←" && e.description == "Collapse the focused task")
        );
        assert!(
            entries
                .iter()
                .any(|e| e.key == "l/→" && e.description == "Expand the focused task")
        );
        assert!(
            entries
                .iter()
                .any(|e| e.key == "E" && e.description == "Expand all tasks")
        );
        assert!(
            entries
                .iter()
                .any(|e| e.key == "C" && e.description == "Collapse all tasks")
        );
    }

    #[test]
    fn help_entries_for_normal_list_should_include_move_and_delete_and_toggle_bindings() {
        let entries = help_entries(&Mode::Normal, Pane::List, DetailField::Title);

        assert!(
            entries
                .iter()
                .any(|e| e.key == "j/↓" && e.description == "Move the selection down")
        );
        assert!(
            entries
                .iter()
                .any(|e| e.key == "dd" && e.description.contains("Delete"))
        );
        assert!(
            entries
                .iter()
                .any(|e| e.key == "x/Space" && e.description == "Toggle complete/incomplete")
        );
    }

    #[test]
    fn help_entries_for_normal_detail_should_describe_i_binding_by_focused_field() {
        let title_entries = help_entries(&Mode::Normal, Pane::Detail, DetailField::Title);
        let description_entries =
            help_entries(&Mode::Normal, Pane::Detail, DetailField::Description);

        assert!(
            title_entries
                .iter()
                .any(|e| e.key == "i" && e.description == "Edit title")
        );
        assert!(
            description_entries
                .iter()
                .any(|e| e.key == "i" && e.description == "Edit description")
        );
    }

    #[test]
    fn help_entries_for_insert_should_include_f1_and_exclude_open_help_from_normal_list() {
        let mode = Mode::Insert {
            field: EditableField::NewTitle,
            buffer: String::new(),
        };

        let entries = help_entries(&mode, Pane::List, DetailField::Title);

        assert!(entries.iter().any(|e| e.key == "F1"));
        assert!(!entries.iter().any(|e| e.key == "?"));
    }

    #[test]
    fn help_entries_for_confirm_should_include_y_and_n_bindings() {
        let mode = Mode::Confirm {
            prompt: "Delete \"Task\"? (y/n)".to_string(),
            action: crate::tui::mode::PendingAction::Delete(bala_core::TaskId::new()),
        };

        let entries = help_entries(&mode, Pane::List, DetailField::Title);

        assert!(entries.iter().any(|e| e.key == "y"));
        assert!(entries.iter().any(|e| e.key == "n/Esc"));
    }

    #[test]
    fn help_entries_for_help_mode_should_return_empty() {
        let mode = Mode::Help {
            previous: Box::new(Mode::Normal),
        };

        let entries = help_entries(&mode, Pane::List, DetailField::Title);

        assert!(entries.is_empty());
    }
}
