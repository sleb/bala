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
    /// selected task). Most prompts accept only `y` (run `action`) or
    /// `n`/`Esc` (cancel back to `Normal`). A
    /// [`PendingAction::DeleteWithChildren`] prompt instead accepts `s`
    /// (delete the subtree), `p` (delete, promoting the children), or
    /// `n`/`Esc`; `y` does nothing there.
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

/// An action awaiting confirmation in `Mode::Confirm`.
///
/// `Delete` is the `dd` sequence on a task with no children, confirmed with
/// `y`/`n`. `DeleteWithChildren` is `dd` on a task that has children, which
/// asks how to treat them: `s` deletes the whole subtree, `p` promotes the
/// children to the task's parents, `n`/`Esc` cancels. `CompleteCascade` is
/// the confirmation for completing a task that has incomplete children
/// (which completes them too). `InheritFromParent` is the prompt shown
/// after creating a new subtask (`o`) whose parent has an assignee and/or
/// dates, asking whether to copy those onto the new `child` from `parent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingAction {
    Delete(TaskId),
    DeleteWithChildren(TaskId),
    CompleteCascade(TaskId),
    InheritFromParent { child: TaskId, parent: TaskId },
}

/// Which pane is focused in `Mode::Normal`.
///
/// `List` is the task-list browsing view; `Detail` is the per-task detail
/// view, opened with `Enter` from
/// `List` and left with `Esc` back to `List`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pane {
    #[default]
    List,
    Detail,
}

/// Which field has the cursor within the Detail pane.
///
/// Only `Title` and `Description` are editable from the Detail pane; dates
/// and assignee are not editable in the TUI yet (use `bala task edit`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DetailField {
    #[default]
    Title,
    Description,
}

/// A field that can be edited via `Mode::Insert`.
///
/// - `NewTitle`: a new top-level task's title (`O`).
/// - `Title(TaskId)`/`Description(TaskId)`: an existing task's
///   title/description, edited from the Detail pane.
/// - `NewSubtaskTitle(TaskId)`: a new subtask's title, under the given
///   parent task id (`o`).
/// - `Parents(TaskId)`: reparenting the given task; the buffer holds a
///   comma-separated list of its parent ids.
/// - `TypeKey(TaskId)`: the given task's `type_key`, edited from the list;
///   the buffer holds the raw type key being typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditableField {
    NewTitle,
    Title(TaskId),
    Description(TaskId),
    NewSubtaskTitle(TaskId),
    Parents(TaskId),
    TypeKey(TaskId),
}
