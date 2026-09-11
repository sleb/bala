# Bala

Bala is a command-line task tracker, written in Rust. Tasks can have
multiple parents, start/due dates, and assignees, all stored in a local
SQLite database.

## Installation

Build from source with Cargo:

```sh
cargo build --release
```

The `bala` binary will be at `target/release/bala`.

## Usage

```sh
# Add a user
bala user add --name "Ada Lovelace"

# Add a task
bala task add --title "Write docs" --due 2026-09-20

# List tasks and users
bala task ls
bala user ls
```

By default, `bala` stores its data in the OS-standard data directory (e.g.
`~/.local/share/bala/bala.db` on Linux). See [docs/cli.md](docs/cli.md) for
the full command reference and [docs/config.md](docs/config.md) for details
on configuring the database path.

## Project layout

This is a Cargo workspace with three crates:

- `bala-core` — domain types shared across the project.
- `bala-store` — SQLite-backed storage layer.
- `bala-cli` — the `bala` command-line interface.

## License

Bala is licensed under the [MIT License](LICENSE).
