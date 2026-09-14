//! Interaction mode, pane, and detail-field cursor for the TUI.
//!
//! `Normal` is the task-list/detail browsing mode; `Insert` is a text-entry
//! mode for creating or editing a field. `Pane` tracks whether the List or
//! Detail view is focused (Detail isn't itself a `Mode` — it's a
//! `Mode::Normal` sub-state, per the LLD's generalization that `Esc` in the
//! Detail pane backs out to the List pane rather than cancelling an edit).
//! `DetailField` tracks which field within the Detail pane has the cursor.
use bala_core::TaskId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert {
        field: EditableField,
        buffer: String,
    },
    /// Confirmation prompt for a destructive/impactful action, entered from
    /// `Normal` (e.g. the second of two consecutive `d` presses on a
    /// selected task). Accepts only `y` (run `action`) or `n`/`Esc` (cancel
    /// back to `Normal`).
    Confirm {
        prompt: String,
        action: PendingAction,
    },
}

/// An action awaiting `y`/`n` confirmation in `Mode::Confirm`.
///
/// Only `Delete` exists for this checkpoint (Story 2.2's `dd` sequence);
/// Story 2.3 is expected to add a completion-cascade variant for tasks with
/// incomplete children.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingAction {
    Delete(TaskId),
}

/// Which pane is focused in `Mode::Normal`.
///
/// `List` is the task-list browsing view (Story 2.1); `Detail` is the
/// per-task detail view this checkpoint adds, opened with `Enter` from
/// `List` and left with `Esc` back to `List`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pane {
    #[default]
    List,
    Detail,
}

/// Which field has the cursor within the Detail pane.
///
/// Only `Title` and `Description` exist for this checkpoint; the full field
/// set (dates, assignee, etc.) is out of scope for Story 2.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DetailField {
    #[default]
    Title,
    Description,
}

/// A field that can be edited via `Mode::Insert`.
///
/// `NewTitle` is Story 2.2 checkpoint 1 (creating a top-level task).
/// `Title(TaskId)`/`Description(TaskId)` are this checkpoint's addition:
/// editing an existing task's title/description from the Detail pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditableField {
    NewTitle,
    Title(TaskId),
    Description(TaskId),
}
