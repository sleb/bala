//! SQLite-backed persistence for Bala tasks.
//!
//! Implements `bala-core`'s `Store`/`StoreTx` trait pair (see
//! `docs/design/data-store.md`) against a single `SQLite` database, opened
//! via [`SqliteStore::open`]/[`SqliteStore::open_in_memory`].
//!
//! ## Schema
//!
//! The schema baseline (`migrations/V1__init.sql`) creates the shape from
//! `docs/design/data-store.md` §Schema, and all of it is in use. Every
//! `tasks` column backs a `Task` field (see the `task` module docs). The
//! `dependency_edges` table is written only through the dependency-edge
//! `StoreTx` methods (see the `edges` module); `get_task`/`list_tasks` read
//! it to fill `Task::depends_on`.
//!
//! ## Error type
//!
//! This crate produces `bala_core::StoreError` values directly rather than
//! defining its own. `StoreError` has a single `Backend(String)` variant,
//! so every `rusqlite`/`refinery` failure is mapped into it via
//! `.to_string()`. Distinguishing e.g. a corrupt-data case from a plain
//! backend failure (per the LLD's proposed taxonomy) would mean adding
//! variants to `StoreError` in `bala-core`.

mod convert;
mod edges;
mod schema;
mod store;
mod task;
mod types;
mod user;

pub use store::SqliteStore;
