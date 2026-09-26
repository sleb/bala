//! Embedded `refinery` migrations (LLD-2 §Schema, §Decision).
//!
//! Migrations live as versioned `.sql` files under `./migrations` (checked
//! into the repo) and are embedded into the binary at compile time via
//! `refinery::embed_migrations!`, then applied automatically whenever a
//! [`crate::SqliteStore`] is opened — see `SqliteStore::open`/
//! `open_in_memory`.

mod embedded {
    use refinery::embed_migrations;

    embed_migrations!("./migrations");
}

pub(crate) use embedded::migrations::runner;
