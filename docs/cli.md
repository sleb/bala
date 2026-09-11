# Bala CLI

`bala` is a command-line task tracker. This document covers the commands
implemented so far.

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

Prints a placeholder message and exits successfully. There is no interactive
TUI yet.

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

Output is one task per line: `<id> <title>`, plus ` (assigned: <name>)` when
the task has an assignee. A task with at least one parent is indented one
level; this is a minimal visual cue, not a full recursive tree layout (that's
planned for the TUI).
