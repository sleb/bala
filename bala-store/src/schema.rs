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

#[cfg(test)]
mod tests {
    use refinery::Target;
    use rusqlite::Connection;

    use super::runner;

    #[test]
    fn migration_v3_should_backfill_positions_by_created_at() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        runner()
            .set_target(Target::Version(2))
            .run(&mut conn)
            .unwrap();

        // ids sort opposite to created_at so the tie-break/ordering is visible.
        let insert_task = |id: u8, created: &str| {
            conn.execute(
                "INSERT INTO tasks (id, title, type_key, status, created_at, updated_at)
                 VALUES (?1, 't', 'task', 'incomplete', ?2, ?2)",
                rusqlite::params![vec![id; 16], created],
            )
            .unwrap();
        };
        insert_task(9, "2026-01-01T00:00:00Z"); // root, oldest
        insert_task(8, "2026-01-02T00:00:00Z"); // root
        insert_task(1, "2026-01-03T00:00:00Z"); // child of 9, newest
        insert_task(2, "2026-01-02T12:00:00Z"); // child of 9, older
        for child in [1u8, 2] {
            conn.execute(
                "INSERT INTO parent_edges (parent_id, child_id) VALUES (?1, ?2)",
                rusqlite::params![vec![9u8; 16], vec![child; 16]],
            )
            .unwrap();
        }

        runner().run(&mut conn).unwrap();

        let ordered = |parent: Option<u8>| -> Vec<(u8, i64)> {
            let mut stmt = conn
                .prepare(
                    "SELECT child_id, position FROM parent_edges
                     WHERE parent_id IS ?1 ORDER BY position",
                )
                .unwrap();
            stmt.query_map([parent.map(|p| vec![p; 16])], |row| {
                Ok((row.get::<_, Vec<u8>>(0)?[0], row.get::<_, i64>(1)?))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect()
        };
        assert_eq!(ordered(None), vec![(9, 0), (8, 1)]);
        assert_eq!(ordered(Some(9)), vec![(2, 0), (1, 1)]);
    }
}
