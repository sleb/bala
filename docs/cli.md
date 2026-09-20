# Bala CLI

`bala` is a command-line task tracker with both a scriptable subcommand set
and an interactive terminal UI. This document covers what's implemented so
far.

## Database location

Commands operate on a SQLite database. By default it lives in the
OS-standard data directory for the `bala` app (e.g.
`~/.local/share/bala/bala.db` on Linux), created automatically on first use.

Override the path with the global `--db-path` flag, mainly useful for tests
or working with multiple task lists:

```sh
bala --db-path ./my-project.db task ls
```

See [config.md](config.md) for the full path-resolution details and default
locations per OS.

## Running with no subcommand

```sh
bala
```

Launches an interactive full-screen terminal UI showing your tasks,
rather than dispatching to one of the subcommands below.

The task list shows every task's title, type, status, and assignee,
indented under its parent(s): a subtask is nested two spaces deeper than
its parent (a task with more than one parent currently appears once under
each of them). A task with subtasks shows a `▾` (expanded) or `▸`
(collapsed) glyph before its checkbox; a collapsed task's row also shows a
`(complete/total)` summary counting its direct children only. Collapse/
expand state persists across sessions in `view.toml` (see
[config.md](config.md)). An empty task list shows a "No tasks yet." message
instead of a blank screen.

| Key | Action |
| --- | --- |
| `j` / `↓` (in the list) | Move the selection down. |
| `k` / `↑` (in the list) | Move the selection up. |
| `h` / `←` (in the list) | Collapse the focused task, hiding its subtasks. No-op if it has none, or is already collapsed. |
| `l` / `→` (in the list) | Expand the focused task, revealing its subtasks again. No-op if it isn't collapsed. |
| `E` (in the list) | Expand every task. |
| `C` (in the list) | Collapse every task that has subtasks. |
| `O` | Create a new top-level task: opens a text-entry line for its title. `Enter` submits, `Esc` cancels. |
| `o` (in the list) | Create a new subtask under the selected task: opens a text-entry line for its title, nested under the focused task. `Enter` submits, `Esc` cancels. If the parent has an assignee or dates, you're then asked whether to inherit them onto the new subtask (`y`/`n`). |
| `m` (in the list) | Reparent the selected task: opens a text-entry line prefilled with its current parent ids (comma-separated). `Enter` submits, `Esc` cancels. |
| `t` (in the list) | Set the selected task's type: opens a text-entry line prefilled with its current type key. `Enter` submits, `Esc` cancels. |
| `f` (in the list) | Cycle the active type filter: no filter → the first configured type → the next → ... → back to no filter. Persists across sessions in `view.toml` (see [config.md](config.md)). |
| `Enter` (in the list) | Open the Detail pane for the selected task. |
| `j` / `↓` (in the Detail pane) | Move the field cursor down (Title → Description). |
| `k` / `↑` (in the Detail pane) | Move the field cursor up (Description → Title). |
| `i` (in the Detail pane) | Edit the field under the cursor: opens a text-entry line prefilled with its current value. `Enter` submits, `Esc` cancels. |
| `Esc` (in the Detail pane) | Leave the Detail pane, back to the list. |
| `dd` (in the list) | Delete the selected task: press `d` twice in a row to prompt for confirmation. `y` confirms, `n`/`Esc` cancels. |
| `x` / `Space` (in the list or Detail pane) | Toggle the selected task's complete/incomplete state. Completing a task with incomplete subtasks prompts for confirmation: `y` completes the whole cascade, `n`/`Esc` cancels. |
| `?` | Open the help overlay, listing every key bound in the current mode. Not available in Insert mode, where `?` types a literal question mark — use `F1` there instead. |
| `F1` (in a text-entry line) | Open the help overlay. |
| `Esc` (in the help overlay) | Close the overlay and return to whatever you were doing. |
| `q` | Quit, restoring the terminal to its normal state. |

Selection stops at the first/last row rather than wrapping around.

Pressing `O` opens a `New task: ` input line at the bottom of the screen.
Type the title and press `Enter` to create it (it's appended to the list and
selected), or `Esc` to cancel without creating anything. An empty title is
rejected with an inline error message shown under the input line; fix the
title and press `Enter` again, or `Esc` to give up.

Pressing `o` on a selected task opens a `New subtask: ` input line at the
bottom of the screen, scoped to that task as the new subtask's parent. Type
the title and press `Enter` to create it — it's nested under the focused
task in the list and selected — or `Esc` to cancel without creating
anything. An empty title is rejected the same way as `O`'s new-task entry.
If the parent has an assignee and/or start/due dates set, submitting the
title then prompts: "Inherit assignee/dates from parent...? (y/n)" — `y`
copies those fields onto the new subtask, `n` leaves it unassigned and
undated. When the parent has none of those fields set, the new subtask is
created directly with no prompt.

Pressing `m` on a selected task opens a `Parents (comma-separated ids): `
input line at the bottom of the screen, prefilled with the task's current
parent ids as a comma-separated list (empty when it's already top-level).
Edit the list and press `Enter` to submit, or `Esc` to cancel without
changing anything. Submitting with an empty buffer promotes the task to
top-level (no parents). An id that isn't a valid task id, or a reparent that
would make the task its own ancestor (a circular hierarchy), shows an inline
error under the input line and keeps you in the entry line to fix it — fix
the list and press `Enter` again, or `Esc` to give up. `m` does not offer to
inherit the new parent's assignee or dates; that's `o`'s behavior for newly
created subtasks only.

Pressing `t` on a selected task opens a `Type: ` input line at the bottom of
the screen, prefilled with the task's current type key. Edit it and press
`Enter` to submit, or `Esc` to cancel without changing anything. A key that
doesn't name a configured task type (see `bala type set`/`bala type ls`)
shows an inline error under the input line and keeps you in the entry line
to fix it — fix the key and press `Enter` again, or `Esc` to give up.

Pressing `Enter` on a task in the list opens its Detail pane, showing the
task's title and description with the field cursor (highlighted) starting on
the title. `j`/`k` move the cursor between the two fields (the same physical
keys used to move the list selection, but scoped to the Detail pane while
it's open); `i` opens a text-entry line prefilled with the focused field's
current value — `Enter` saves the change and returns to the Detail pane
showing the new value, `Esc` discards it. An empty title is rejected the
same way as in `O`'s new-task entry (inline error, stays in the entry line);
an empty description is accepted. `Esc` in the Detail pane itself (not
mid-edit) leaves it and returns to the list.

Pressing `d` twice in a row on a selected task shows a confirmation prompt
naming the task, at the bottom of the screen: press `y` to delete it (along
with its subtasks, if it has any), or `n`/`Esc` to cancel without deleting
anything. Pressing `d` once and then any other key (rather than a second
`d`) does not count toward the sequence — the next `d` starts fresh.

The TUI requires a real terminal (a TTY) on stdin: running it in a
non-interactive context (e.g. piped/redirected input, as CI or scripting
harnesses do) fails immediately with a "terminal I/O error" message and a
nonzero exit code, rather than hanging.

## `bala user`

### `bala user add`

Creates a new user.

```sh
bala user add --name "Ada Lovelace"
```

| Flag | Required | Description |
| --- | --- | --- |
| `--name` | yes | The user's display name. |

Prints the new user's id (a UUID) on success.

### `bala user ls`

Lists all users.

```sh
bala user ls
```

Output is one user per line: `<id> <name>`.

## `bala task`

### `bala task add`

Creates a new task.

```sh
bala task add --title "Write docs" \
  --description "Document the CLI commands" \
  --parent <parent-task-id> \
  --start 2026-09-15 \
  --due 2026-09-20 \
  --assignee <user-id> \
  --type task
```

| Flag | Required | Description |
| --- | --- | --- |
| `--title` | yes | The task's title. |
| `--description` | no | A longer description of the task. |
| `--parent` | no (repeatable) | Id of a parent task. May be given multiple times to attach the new task under several parents. |
| `--start` | no | Start date, `YYYY-MM-DD`. |
| `--due` | no | Due date, `YYYY-MM-DD`. |
| `--assignee` | no | Id of the user to assign the task to. |
| `--inherit` | no | Copy assignee/start/due from the first `--parent` for any of those fields not explicitly given. Only meaningful alongside `--parent`; passing it without `--parent` is a usage error. |
| `--type` | no | Task type key. Defaults to the seeded `task` type when omitted. Fails with a nonzero exit if the key doesn't name a configured task type. |

`--inherit` fills in `--assignee`/`--start`/`--due` from the new task's
first `--parent` wherever the corresponding flag wasn't given explicitly —
an explicit flag always wins. With multiple `--parent` values, only the
first one is consulted.

Prints the new task's id (a UUID) on success.

### `bala task ls`

Lists all tasks.

```sh
bala task ls
bala task ls --type task
```

| Flag | Required | Description |
| --- | --- | --- |
| `--type` | no | Only show tasks with this type key. Filtering by a key no task currently has (including one that isn't configured) simply shows no tasks — it's a plain filter, not an existence check. |

Output is one task per line: `[<marker>] <id> <title> (type: <type-key>)`,
plus ` (assigned: <name>)` when the task has an assignee. `<marker>` is `x`
for a completed task and a space otherwise. A task with at least one parent
is indented one level; this is a minimal visual cue, not a full recursive
tree layout (that's planned for the TUI).

### `bala task edit`

Edits an existing task. Only the fields you pass are changed; everything
else is left as-is.

```sh
bala task edit <task-id> \
  --title "Write docs" \
  --description "Document the CLI commands" \
  --start 2026-09-15 \
  --due 2026-09-20 \
  --assignee <user-id> \
  --type <type-key>
```

| Flag | Description |
| --- | --- |
| `--title` | New title. |
| `--description` | New description. |
| `--clear-description` | Clear the description. Conflicts with `--description`. |
| `--start` | New start date, `YYYY-MM-DD`. |
| `--clear-start` | Clear the start date. Conflicts with `--start`. |
| `--due` | New due date, `YYYY-MM-DD`. |
| `--clear-due` | Clear the due date. Conflicts with `--due`. |
| `--assignee` | Id of the user to assign the task to. |
| `--clear-assignee` | Unassign the task. Conflicts with `--assignee`. |
| `--type` | New task type key. |

Prints the edited task (and any other tasks affected by the change), one
per line in the same format as `task ls`.

### `bala task mv`

Reparents a task under a new set of parents, replacing its current parents
entirely.

```sh
bala task mv <task-id> --parents <parent-id>[,<parent-id>...]
```

| Flag | Required | Description |
| --- | --- | --- |
| `--parents` | no (comma-separated) | New parent ids, comma-separated. Omit to promote the task to top-level (no parents). |

Prints the reparented task, in the same format as `task ls`.

If the task id or any given parent id doesn't exist, or if the move would
make the task its own ancestor (for example naming the task itself, or one
of its own descendants, as a new parent), the command fails with a nonzero
exit and an error message describing the problem.

### `bala task delete`

Deletes a task, prompting for confirmation unless `--yes` is given.

```sh
bala task delete <task-id> [--yes] [--cascade | --promote-children]
```

| Flag | Description |
| --- | --- |
| `--yes` | Skip the interactive confirmation prompt. |
| `--cascade` | Delete the task's whole subtree along with it. Conflicts with `--promote-children`. |
| `--promote-children` | Reattach the task's children to its own parents instead of deleting them. |

If the task has subtasks and neither `--cascade` nor `--promote-children` is
given, the command fails and lists the subtasks so you can choose a mode.

Prints the id of each deleted task, one per line, on success.

### `bala task restore`

Restores a previously deleted task.

```sh
bala task restore <task-id>
```

Prints the restored task, in the same format as `task ls`.

### `bala task complete`

Marks a task complete.

```sh
bala task complete <task-id> [--cascade]
```

| Flag | Description |
| --- | --- |
| `--cascade` | Also complete every incomplete descendant. |

Prints the completed task (and, with `--cascade`, each descendant it also
completed), one per line in the same format as `task ls`.

### `bala task reopen`

Reopens a previously completed task, marking it incomplete again.

```sh
bala task reopen <task-id>
```

Prints the reopened task, in the same format as `task ls`.

## `bala type`

Configures the list of task types available to `task add`/`task edit`'s
`--type` flag. A fresh database is seeded with one default type, key
`task`.

### `bala type ls`

Lists all configured task types.

```sh
bala type ls
```

Output is one type per line: `<key> <label> [color=<color>]
sort_order=<n>`. The `color=<color>` segment is only printed when the type
has a color set.

### `bala type set`

Creates a new task type, or updates an existing one by key.

```sh
bala type set <key> --label <label> [--color <color>] [--sort-order <n>]
```

| Flag | Required | Description |
| --- | --- | --- |
| `--label` | yes | The type's display label. |
| `--color` | no | A color for the type (any string; interpretation is left to consumers such as the TUI). |
| `--sort-order` | no | An integer used to order types relative to each other. |

When `<key>` already names an existing type, `label` is always overwritten
with the given value, but an omitted `--color`/`--sort-order` keeps that
type's current value rather than clearing it. When `<key>` is new,
an omitted `--color` leaves it unset and an omitted `--sort-order`
defaults to `0`.

Prints the resulting type, in the same one-line format as `type ls`.
