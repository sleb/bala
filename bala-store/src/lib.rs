//! SQLite-backed persistence for Bala tasks.
//!
//! Implements `bala-core`'s `Store`/`StoreTx` trait pair (see
//! `docs/design/data-store.md`) against a single SQLite database, opened
//! via [`SqliteStore::open`]/[`SqliteStore::open_in_memory`].
//!
//! ## Unused schema
//!
//! The schema baseline (`migrations/V1__init.sql`) creates the full shape
//! from `docs/design/data-store.md` §Schema, so adding task dependencies
//! needs no schema change. Until then the
//! `dependency_edges` table and the `tasks.out_of_sync` column sit unused
//! — `bala-core`'s `Store` trait has no dependency methods and `Task` has
//! no `out_of_sync` field (see the `edges` and `task` module docs).
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
