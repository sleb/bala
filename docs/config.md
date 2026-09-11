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

## What's not configurable yet

There's no `view.toml` or persisted view/UI state, and no user-editable
config file — just enough plumbing to find (and create) a directory for
`bala.db`. This will grow as the TUI and other settings land.
