-- Story 1.1a: add a `users` table and give `tasks.assignee_id` a real FK
-- to it, per docs/design/data-store.md's "migrations/V2__add_users.sql
-- (Story 1.1a)" section.
--
-- SQLite can't `ALTER TABLE ... ADD COLUMN ... REFERENCES ...` — a
-- `REFERENCES` clause can only be declared when a column is first
-- created, and `tasks.assignee_id` already exists (untyped BLOB, no FK)
-- from V1. Giving it the FK means the standard SQLite "add a constraint
-- to an existing column" rebuild: create `users`; rename `tasks` out of
-- the way; create a new `tasks` identical to V1's except
-- `assignee_id BLOB REFERENCES users(id)`; copy every row across; drop
-- the renamed-away old table; recreate the three `tasks` indexes, since
-- `DROP TABLE` takes its indexes with it. No existing row can violate the
-- new FK (V1 never populated `assignee_id`), so the copy needs no data
-- migration beyond a straight copy.
--
-- `parent_edges`/`dependency_edges` reference `tasks(id)` via FK. This was
-- verified experimentally to matter: an earlier draft of this migration
-- renamed `tasks` to `tasks_old` first (the "obvious" order) and every
-- test that both `put_task`s and `add_parent_edge`s failed with `no such
-- table: main.tasks_old`. The cause is a documented SQLite `ALTER TABLE
-- RENAME` behavior — renaming a table that other tables reference via FK
-- automatically rewrites *their* schema's `REFERENCES` clause to the new
-- name, so `parent_edges`/`dependency_edges` silently became
-- `REFERENCES tasks_old(id)`; once `tasks_old` was later dropped, those
-- FKs pointed at nothing. The fix (this file's actual order, matching
-- SQLite's own recommended 12-step "make complex schema changes" recipe):
-- build the replacement table under a fresh temporary name, copy the
-- data across, drop the *original* `tasks` (which `parent_edges`/
-- `dependency_edges` still reference by that same name throughout, so no
-- auto-rewrite is triggered), then rename the temporary table into the
-- now-free `tasks` name. `parent_edges`/`dependency_edges` never get
-- renamed themselves, so this is the one and only table-identity change
-- their FKs need to track. Re-ran the full `cargo test -p bala-store`
-- suite (including tests that add parent edges alongside task rows)
-- against this order and it passes with no `PRAGMA foreign_keys=OFF`
-- needed.

CREATE TABLE users (
    id    BLOB PRIMARY KEY,   -- 16-byte UUID
    name  TEXT NOT NULL
);

CREATE TABLE tasks_new (
    id            BLOB PRIMARY KEY,          -- 16-byte UUID
    title         TEXT NOT NULL,
    description   TEXT,
    type_key      TEXT NOT NULL REFERENCES task_types(key),
    status        TEXT NOT NULL CHECK (status IN ('incomplete', 'complete')),
    start_date    TEXT,                      -- ISO-8601 date
    due_date      TEXT,
    assignee_id   BLOB REFERENCES users(id),
    out_of_sync   INTEGER NOT NULL DEFAULT 0, -- 0/1
    created_at    TEXT NOT NULL,              -- RFC 3339 UTC
    updated_at    TEXT NOT NULL,
    completed_at  TEXT,
    deleted_at    TEXT                        -- soft-delete tombstone; NULL = live
);

INSERT INTO tasks_new (
    id, title, description, type_key, status, start_date, due_date,
    assignee_id, out_of_sync, created_at, updated_at, completed_at, deleted_at
)
SELECT
    id, title, description, type_key, status, start_date, due_date,
    assignee_id, out_of_sync, created_at, updated_at, completed_at, deleted_at
FROM tasks;

DROP TABLE tasks;

ALTER TABLE tasks_new RENAME TO tasks;

CREATE INDEX idx_tasks_type_live     ON tasks(type_key)     WHERE deleted_at IS NULL;
CREATE INDEX idx_tasks_status_live   ON tasks(status)       WHERE deleted_at IS NULL;
CREATE INDEX idx_tasks_assignee_live ON tasks(assignee_id)  WHERE deleted_at IS NULL;
