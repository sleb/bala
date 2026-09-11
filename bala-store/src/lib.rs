//! SQLite-backed persistence for Bala tasks.
//!
//! Implements `bala-core`'s `Store`/`StoreTx` trait pair (see
//! `docs/design/data-store.md`) against a single SQLite database, opened
//! via [`SqliteStore::open`]/[`SqliteStore::open_in_memory`].
//!
//! ## Scope of this crate at this checkpoint
//!
//! `bala-core`'s current `Store`/`StoreTx` trait only declares 7 methods
//! (`get_task`, `put_task`, `list_tasks`, `list_parent_edges`,
//! `add_parent_edge`, `get_task_types`, `put_task_type`) — the full LLD
//! contract's `list_child_edges`, `remove_parent_edge`, and the
//! dependency-edge methods aren't implemented here because `bala-core`
//! doesn't declare them yet; they land in later stories that extend the
//! trait.
//!
//! The SQLite schema (`migrations/V1__init.sql`) is nonetheless the full
//! 4-table shape from `docs/design/data-store.md` §Schema, `dependency_edges`
//! included, so no breaking migration is needed when those later stories
//! land. A few `tasks` columns exist in the schema for that same
//! forward-compat reason but are unused by `bala-core`'s current `Task`
//! shape: `assignee_id`, `out_of_sync`, `completed_at`, `deleted_at` are
//! always written as `NULL`/`0` by `put_task` and never read back into a
//! `Task` (which has no field for them yet) — see the `task` module's docs.
//!
//! ## Error type
//!
//! This crate produces `bala_core::StoreError` values directly rather than
//! defining its own — `bala-core`'s current `StoreError` is a
//! Checkpoint-1 placeholder with a single `Backend(String)` variant, so
//! every `rusqlite`/`refinery` failure is mapped into it via
//! `.to_string()`. This keeps `bala-core` untouched per this checkpoint's
//! scope; a richer `StoreError` (distinguishing e.g. a corrupt-data case
//! from a plain backend failure, per the LLD's proposed taxonomy) is
//! something a future checkpoint may want to raise back to `bala-core`,
//! but isn't required to make this checkpoint's trait impl correct.

mod convert;
mod edges;
mod schema;
mod store;
mod task;
mod types;
mod user;

pub use store::SqliteStore;
