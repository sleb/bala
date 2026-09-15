# Bala Configuration

This document covers how `bala` is configured today. For command usage, see
[cli.md](cli.md).

## Database path resolution

`bala` needs to know where its SQLite database lives. Resolution order:

1. The global `--db-path` flag, if given (see
   [Overriding the path](#overriding-the-path) below).
2. Otherwise, the OS-standard data directory for the `bala` app.

There is no config file and no environment variable support yet — the only
per-invocation override is the `--db-path` flag.

### Default location

When `--db-path` is not given, `bala` resolves a per-OS data directory (via
the [`directories`](https://docs.rs/directories) crate's `ProjectDirs`) and
appends `bala.db`:

| OS | Default path |
| --- | --- |
| Linux | `~/.local/share/bala/bala.db` |
| macOS | `~/Library/Application Support/bala/bala.db` |
| Windows | `%APPDATA%\bala\data\bala.db` |

The containing directory is created automatically if it doesn't already
exist. If no data directory can be determined for the current OS, or it
can't be created, `bala` exits with an error before running any command.

### Overriding the path

Pass `--db-path` before the subcommand to use a different file, e.g. for
tests or to keep multiple task lists:

```sh
bala --db-path ./my-project.db task ls
```

When `--db-path` is set, `bala` uses that path exactly as given and does not
create any directories on its own — see [cli.md](cli.md#database-location)
for the flag reference and command examples.

## View state (view.toml)

The TUI remembers which task was selected in the tree, and restores it the
next time you open `bala`'s TUI against the same database.

### Path resolution

There's no flag to override this path — it's always the OS-standard config
directory for the `bala` app (again via the
[`directories`](https://docs.rs/directories) crate's `ProjectDirs`, this time
its config dir rather than its data dir), with `view.toml` appended:

| OS | View state path |
| --- | --- |
| Linux | `~/.config/bala/view.toml` |
| macOS | `~/Library/Application Support/bala/view.toml` |
| Windows | `%APPDATA%\bala\config\view.toml` |

The containing directory is created automatically if it doesn't already
exist, the same as for `bala.db`.

### What's persisted

Today, only the selected task (stored as `selected = "<uuid>"` under a
`[tree]` table). Other view state described in the design (collapsed nodes,
Gantt scale/anchor, filters, "blocked only") isn't persisted yet — those are
expected to join later as additional keys in `[tree]`, or in sibling tables,
without breaking this file's format.

### Missing or corrupt file

Unlike the database path, a problem with `view.toml` never blocks the TUI
from starting. If the file doesn't exist yet, can't be read, or doesn't
parse as valid TOML, `bala` silently falls back to no selection and starts
normally.

The file itself is written atomically (a temp file, then renamed into
place) when the TUI quits, so an interrupted write can't leave `view.toml`
corrupt for the next run. A failure to save (e.g. an unwritable config
directory) prints a warning to stderr but doesn't stop `bala` from quitting.

## What's not configurable yet

There's no user-editable config file — `bala` only writes `view.toml` for
itself to persist TUI view state, and there's no flag or environment
variable to override where it lives (unlike `--db-path` for the database).
This will grow as the TUI and other settings land.
