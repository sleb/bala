//! `users` CRUD.

use bala_core::{StoreError, User, UserId};
use rusqlite::{OptionalExtension, Transaction, params};

use crate::convert::{blob_to_user_id, user_id_to_blob};
use crate::task::sqlite_err;

pub(crate) fn get_user(tx: &Transaction, id: UserId) -> Result<Option<User>, StoreError> {
    let id_blob = user_id_to_blob(id);
    let found = tx
        .query_row(
            "SELECT id, name FROM users WHERE id = ?1",
            params![id_blob.as_slice()],
            |row| {
                let id_blob: Vec<u8> = row.get(0)?;
                let name: String = row.get(1)?;
                Ok((id_blob, name))
            },
        )
        .optional()
        .map_err(sqlite_err)?;

    found
        .map(|(id_blob, name)| {
            Ok(User {
                id: blob_to_user_id(&id_blob)?,
                name,
            })
        })
        .transpose()
}

/// Upserts by `id` (matching `types::put_task_type`'s `ON CONFLICT` style).
pub(crate) fn put_user(tx: &Transaction, user: &User) -> Result<(), StoreError> {
    let id_blob = user_id_to_blob(user.id);
    tx.execute(
        "INSERT INTO users (id, name) VALUES (?1, ?2)
        ON CONFLICT(id) DO UPDATE SET name = excluded.name",
        params![id_blob.as_slice(), user.name],
    )
    .map_err(sqlite_err)?;
    Ok(())
}

pub(crate) fn list_users(tx: &Transaction) -> Result<Vec<User>, StoreError> {
    let mut stmt = tx
        .prepare("SELECT id, name FROM users")
        .map_err(sqlite_err)?;
    let rows = stmt
        .query_map([], |row| {
            let id_blob: Vec<u8> = row.get(0)?;
            let name: String = row.get(1)?;
            Ok((id_blob, name))
        })
        .map_err(sqlite_err)?;

    let mut users = Vec::new();
    for row in rows {
        let (id_blob, name) = row.map_err(sqlite_err)?;
        users.push(User {
            id: blob_to_user_id(&id_blob)?,
            name,
        });
    }
    Ok(users)
}
