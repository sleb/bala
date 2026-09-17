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
    /// The keybinding help overlay, entered from any other mode with `?`
    /// (or `F1` from `Insert`) and left with `Esc`, restoring `previous`.
    Help {
        previous: Box<Mode>,
    },
}

/// An action awaiting `y`/`n` confirmation in `Mode::Confirm`.
///
/// `Delete` is Story 2.2's `dd` sequence; `CompleteCascade` is Story 2.3's
/// confirmation for completing a task that has incomplete children.
/// `InheritFromParent` is Story 3.1's prompt, shown after creating a new
/// subtask (`o`) whose parent has an assignee and/or dates, asking whether
/// to copy those onto the new `child` from `parent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingAction {
    Delete(TaskId),
    CompleteCascade(TaskId),
    InheritFromParent { child: TaskId, parent: TaskId },
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
/// `NewSubtaskTitle(TaskId)` is Story 3.1's addition: creating a new
/// subtask's title, scoped to the given parent task id. `Parents(TaskId)` is
/// Story 3.1's other addition: reparenting an existing task (the given task
/// id), whose buffer holds a comma-separated list of the task's parent ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditableField {
    NewTitle,
    Title(TaskId),
    Description(TaskId),
    NewSubtaskTitle(TaskId),
    Parents(TaskId),
}
