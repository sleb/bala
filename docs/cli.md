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

Launches an interactive full-screen terminal UI showing your top-level
tasks, rather than dispatching to one of the subcommands below.

The task list shows each top-level task's title, type, status, and
assignee (nested/tree rendering is planned for a later release; for now
subtasks are simply not shown here — use `bala task ls` to see everything,
including subtasks). An empty task list shows a "No tasks yet." message
instead of a blank screen.

| Key | Action |
| --- | --- |
| `j` / `↓` | Move the selection down. |
| `k` / `↑` | Move the selection up. |
| `q` | Quit, restoring the terminal to its normal state. |

Selection stops at the first/last row rather than wrapping around.

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
  --assignee <user-id>
```

| Flag | Required | Description |
| --- | --- | --- |
| `--title` | yes | The task's title. |
| `--description` | no | A longer description of the task. |
| `--parent` | no (repeatable) | Id of a parent task. May be given multiple times to attach the new task under several parents. |
| `--start` | no | Start date, `YYYY-MM-DD`. |
| `--due` | no | Due date, `YYYY-MM-DD`. |
| `--assignee` | no | Id of the user to assign the task to. |

Prints the new task's id (a UUID) on success.

### `bala task ls`

Lists all tasks.

```sh
bala task ls
```

Output is one task per line: `[<marker>] <id> <title>`, plus
` (assigned: <name>)` when the task has an assignee. `<marker>` is `x` for a
completed task and a space otherwise. A task with at least one parent is
indented one level; this is a minimal visual cue, not a full recursive tree
layout (that's planned for the TUI).

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
